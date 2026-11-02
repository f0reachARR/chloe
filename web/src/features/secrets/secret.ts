// シークレットの定義 (SecretSpec) とフォームの値、登録の本文 (SecretInput) とのあいだの変換。
import type { Schemas } from '@/shared/api/client';

type SecretSpec = Schemas['SecretSpec'];
type SecretInput = Schemas['SecretInput'];

// 欄が一つのシークレットの欄の名前。
export const VALUE = 'value';

// 署名の方式が求める欄と、あれば使う欄 (`SignKind::fields` と同じ)。
export const signFields: Record<Schemas['SignKind'], { required: string[]; optional: string[] }> = {
  'aws-sigv4': {
    required: ['access_key_id', 'secret_access_key'],
    optional: ['session_token'],
  },
};

export interface FieldValues {
  // React の key。送らない。
  key: string;
  name: string;
  env: string[];
  // 空なら送らず、`{id}` になる。
  placeholder: string;
  value: string;
}

export interface InjectValues {
  key: string;
  field: string;
  hosts: string[];
  header: string;
  format: Schemas['InjectFormat'];
  // 空ならユーザー名を差し替えない。
  usernameField: string;
}

export interface SignValues {
  key: string;
  scheme: Schemas['SignKind'];
  hosts: string[];
}

export interface FileValues {
  key: string;
  path: string;
  content: string;
  owner: Schemas['FileOwner'];
  // 空なら送らず、0644 になる。
  mode: string;
}

export interface SecretValues {
  name: string;
  fields: FieldValues[];
  inject: InjectValues[];
  sign: SignValues[];
  files: FileValues[];
}

let nextKey = 0;
export const newKey = () => `k${nextKey++}`;

export function newField(name = ''): FieldValues {
  return { key: newKey(), name, env: [], placeholder: '', value: '' };
}

export function newInject(field = VALUE): InjectValues {
  return {
    key: newKey(),
    field,
    hosts: [],
    header: 'authorization',
    format: 'replace',
    usernameField: '',
  };
}

export function newSign(): SignValues {
  return { key: newKey(), scheme: 'aws-sigv4', hosts: [] };
}

export function newFile(): FileValues {
  return { key: newKey(), path: '', content: '', owner: 'root', mode: '' };
}

export function emptySecret(): SecretValues {
  return { name: '', fields: [newField(VALUE)], inject: [newInject()], sign: [], files: [] };
}

// 定義をフォームの値にする。値は読めないので空にする (定義を変えるときは入れ直す)。
export function fromSpec(name: string, spec: SecretSpec): SecretValues {
  return {
    name,
    fields: Object.entries(spec.fields).map(([field, f]) => ({
      key: newKey(),
      name: field,
      env: f.env ?? [],
      placeholder: f.placeholder,
      value: '',
    })),
    inject: (spec.inject ?? []).map((rule) => ({
      key: newKey(),
      field: rule.field ?? VALUE,
      hosts: rule.hosts,
      header: rule.header,
      format: rule.format ?? 'replace',
      usernameField: rule.username_field ?? '',
    })),
    sign: (spec.sign ?? []).map((rule) => ({
      key: newKey(),
      scheme: rule.scheme,
      hosts: rule.hosts,
    })),
    files: (spec.files ?? []).map((file) => ({
      key: newKey(),
      path: file.path,
      content: file.content,
      owner: file.owner ?? 'root',
      mode: file.mode ?? '',
    })),
  };
}

// 欄が `value` 一つだけか (そのときは本文の上に直に書く)。
export function isSingle(fields: { name: string }[]): boolean {
  return fields.length === 1 && fields[0].name === VALUE;
}

// 登録の本文。定義のすべてと、すべての欄の値を送る。
export function toSecretInput(values: SecretValues): SecretInput {
  const input: SecretInput = {};
  if (isSingle(values.fields)) {
    const [field] = values.fields;
    input.env = field.env;
    if (field.placeholder !== '') input.placeholder = field.placeholder;
    input.value = field.value;
  } else {
    input.fields = Object.fromEntries(
      values.fields.map((field) => [
        field.name,
        {
          env: field.env,
          value: field.value,
          ...(field.placeholder !== '' && { placeholder: field.placeholder }),
        },
      ]),
    );
  }
  input.inject = values.inject.map((rule) => ({
    field: rule.field,
    hosts: rule.hosts,
    header: rule.header,
    format: rule.format,
    ...(rule.format === 'basic-password' &&
      rule.usernameField !== '' && { username_field: rule.usernameField }),
  }));
  input.sign = values.sign.map((rule) => ({ scheme: rule.scheme, hosts: rule.hosts }));
  input.files = values.files.map((file) => ({
    path: file.path,
    content: file.content,
    ...(file.owner !== 'root' && { owner: file.owner }),
    ...(file.mode !== '' && { mode: file.mode }),
  }));
  return input;
}

// 値だけを替える本文。組み込みの種類と欄が一つのものは値だけを送る (登録してある定義を使う)。
// 欄がいくつもあるものは、API が欄ごとの値を定義と一緒にしか受けないので、今の定義を添える。
export function toValueInput(
  name: string,
  spec: SecretSpec,
  builtin: boolean,
  values: Record<string, string>,
): SecretInput {
  if (builtin || isSingle(Object.keys(spec.fields).map((field) => ({ name: field })))) {
    return { value: values[VALUE] ?? '' };
  }
  const form = fromSpec(name, spec);
  form.fields = form.fields.map((field) => ({ ...field, value: values[field.name] ?? '' }));
  return toSecretInput(form);
}

// 差し替えと署名の宛先 (一覧に出す)。
export function secretHosts(spec: SecretSpec): string[] {
  const hosts = [
    ...(spec.inject ?? []).flatMap((rule) => rule.hosts),
    ...(spec.sign ?? []).flatMap((rule) => rule.hosts),
  ];
  return [...new Set(hosts)];
}

// VM に入る環境変数 (一覧に出す)。
export function secretEnv(spec: SecretSpec): string[] {
  return Object.values(spec.fields).flatMap((field) => field.env ?? []);
}

// シークレットと欄の名前 (`check_name` と同じ。英小文字で始まり、英小文字、数字、`_`)。
export function validateSecretName(name: string): string | null {
  if (name === '') return 'Enter a name';
  if (
    name.length > 64 ||
    !/^[a-z][a-z0-9_]*$/.test(name) ||
    name.includes('__') ||
    name.endsWith('_')
  ) {
    return 'Use lowercase letters, digits and single _ (start with a letter)';
  }
  return null;
}
