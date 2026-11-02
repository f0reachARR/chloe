// 新しいワークスペースのフォームの値と、`POST /api/v1/workspaces` の本文とのあいだの変換。
import type { Schemas } from '@/shared/api/client';

import { idlePattern } from './machine';

type CreateWorkspace = Schemas['CreateWorkspace'];

export interface CreateValues {
  // `user:alice`、`org:acme`、`team:acme/dev`。
  owner: string;
  name: string;
  // `default` なら送らず、持ち主の設定の既定に従う。
  root: 'default' | Schemas['RootMode'];
  // 空 ('') なら送らず、ゲートウェイの既定に従う。
  vcpus: number | '';
  memoryGib: number | '';
  workspaceDiskGib: number | '';
  dockerDiskGib: number | '';
  idle: 'default' | 'after' | 'never';
  idleAfter: string;
}

export function emptyCreateValues(owner: string): CreateValues {
  return {
    owner,
    name: '',
    root: 'default',
    vcpus: '',
    memoryGib: '',
    workspaceDiskGib: '',
    dockerDiskGib: '',
    idle: 'default',
    idleAfter: '30m',
  };
}

// ゲートウェイの `validate_name` と同じ。
export const namePattern = /^[A-Za-z0-9_][A-Za-z0-9_-]{0,63}$/;

export function validateName(name: string): string | null {
  if (name === '') return 'Enter a name';
  if (!namePattern.test(name)) {
    return 'Use up to 64 letters, digits, - and _ (not starting with -)';
  }
  return null;
}

export function validateIdle(values: CreateValues): string | null {
  return values.idle !== 'after' || idlePattern.test(values.idleAfter)
    ? null
    : 'Use e.g. 30m, 2h or 1d';
}

// SSH で使う名前 (`web`、`acme/web`、`acme/dev/web`)。ゲートウェイは名前を小文字にする。
export function fullName(prefix: string, name: string): string {
  return `${prefix}${name.toLowerCase()}`;
}

// 書いた項目だけを送る。
export function toCreateBody(prefix: string, values: CreateValues): CreateWorkspace {
  const body: CreateWorkspace = { name: fullName(prefix, values.name) };
  if (values.root !== 'default') body.root = values.root;
  if (values.vcpus !== '') body.vcpus = values.vcpus;
  if (values.memoryGib !== '') body.memory_mib = Math.round(values.memoryGib * 1024);
  if (values.workspaceDiskGib !== '') body.workspace_disk_gib = values.workspaceDiskGib;
  if (values.dockerDiskGib !== '') body.docker_disk_gib = values.dockerDiskGib;
  if (values.idle === 'after') body.idle_timeout = values.idleAfter;
  if (values.idle === 'never') body.idle_timeout = 'never';
  return body;
}
