import { describe, expect, it } from 'vitest';

import type { Schemas } from '@/shared/api/client';

import {
  emptySecret,
  fromSpec,
  newField,
  newFile,
  newInject,
  newSign,
  secretHosts,
  toSecretInput,
  toValueInput,
  validateSecretName,
} from './secret';

const aws: Schemas['SecretSpec'] = {
  fields: {
    access_key_id: { env: ['AWS_ACCESS_KEY_ID'], placeholder: 'AKIA{id}' },
    secret_access_key: { env: ['AWS_SECRET_ACCESS_KEY'], placeholder: '{id}' },
  },
  sign: [{ scheme: 'aws-sigv4', hosts: ['*.amazonaws.com'] }],
};

describe('toSecretInput', () => {
  it('writes a single value field at the top', () => {
    const values = emptySecret();
    values.name = 'hf_token';
    values.fields[0] = { ...values.fields[0], env: ['HF_TOKEN'], value: 'hf_real' };
    values.inject[0] = { ...values.inject[0], hosts: ['huggingface.co'] };
    expect(toSecretInput(values)).toEqual({
      env: ['HF_TOKEN'],
      value: 'hf_real',
      inject: [
        { field: 'value', hosts: ['huggingface.co'], header: 'authorization', format: 'replace' },
      ],
      sign: [],
      files: [],
    });
  });

  it('writes several fields, user names, signing and files', () => {
    const values = {
      name: 'registry',
      fields: [
        { ...newField('user'), placeholder: 'user{id}', value: 'alice' },
        { ...newField('password'), env: ['REG_PASSWORD'], value: 'p@ss' },
      ],
      inject: [
        {
          ...newInject('password'),
          hosts: ['registry.example.com'],
          format: 'basic-password' as const,
          usernameField: 'user',
        },
      ],
      sign: [{ ...newSign(), hosts: ['*.amazonaws.com'] }],
      files: [
        { ...newFile(), path: '/etc/x.conf', content: 'login {user} {password}', mode: '0600' },
      ],
    };
    expect(toSecretInput(values)).toEqual({
      fields: {
        user: { env: [], placeholder: 'user{id}', value: 'alice' },
        password: { env: ['REG_PASSWORD'], value: 'p@ss' },
      },
      inject: [
        {
          field: 'password',
          hosts: ['registry.example.com'],
          header: 'authorization',
          format: 'basic-password',
          username_field: 'user',
        },
      ],
      sign: [{ scheme: 'aws-sigv4', hosts: ['*.amazonaws.com'] }],
      files: [{ path: '/etc/x.conf', content: 'login {user} {password}', mode: '0600' }],
    });
  });

  it('round-trips a definition, leaving the values empty', () => {
    const form = fromSpec('aws', aws);
    expect(form.fields.map((f) => [f.name, f.value])).toEqual([
      ['access_key_id', ''],
      ['secret_access_key', ''],
    ]);
    const { fields, ...rest } = toSecretInput(form);
    expect(Object.keys(fields ?? {})).toEqual(['access_key_id', 'secret_access_key']);
    expect(rest.sign).toEqual(aws.sign);
  });
});

describe('toValueInput', () => {
  it('sends only the value for single and built-in secrets', () => {
    const single = { fields: { value: { placeholder: 'sk-{id}' } } };
    expect(toValueInput('key', single, false, { value: 'new' })).toEqual({ value: 'new' });
    expect(toValueInput('github_token', single, true, { value: 'ghp' })).toEqual({ value: 'ghp' });
  });

  it('sends the current definition with every field for several fields', () => {
    const input = toValueInput('aws', aws, false, {
      access_key_id: 'AKIAREAL',
      secret_access_key: 'shh',
    });
    expect(input.fields).toEqual({
      access_key_id: { env: ['AWS_ACCESS_KEY_ID'], placeholder: 'AKIA{id}', value: 'AKIAREAL' },
      secret_access_key: { env: ['AWS_SECRET_ACCESS_KEY'], placeholder: '{id}', value: 'shh' },
    });
    expect(input.sign).toEqual(aws.sign);
  });
});

describe('helpers', () => {
  it('lists hosts once and checks names like the gateway', () => {
    expect(
      secretHosts({
        fields: {},
        inject: [{ hosts: ['a.example', 'b.example'], header: 'x' }],
        sign: [{ scheme: 'aws-sigv4', hosts: ['a.example'] }],
      }),
    ).toEqual(['a.example', 'b.example']);
    for (const ok of ['hf_token', 'a1']) expect(validateSecretName(ok)).toBeNull();
    for (const bad of ['', 'Hf', '1a', 'a__b', 'a_', 'a-b']) {
      expect(validateSecretName(bad)).not.toBeNull();
    }
  });
});
