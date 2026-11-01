// Machine のセクションのフォームの値と、管理 API の値とのあいだの変換。
import type { Schemas } from '@/shared/api/client';

type WorkspaceInfo = Schemas['WorkspaceInfo'];
type UpdateWorkspace = Schemas['UpdateWorkspace'];

export interface MachineValues {
  vcpus: number;
  memoryGib: number;
  root: Schemas['RootMode'];
  workspaceDiskGib: number;
  dockerDiskGib: number;
  // 止めるまでの時間。API は既定を当てはめた値しか返さないので、読んだ値は `after` か `never` になる。
  idle: 'after' | 'never' | 'default';
  idleAfter: string;
}

// 秒を、`chloe ws idle` と同じ書き方 (30m、2h、1d) にする。
export function formatIdle(secs: number): string {
  for (const [unit, size] of [
    ['d', 86400],
    ['h', 3600],
    ['m', 60],
  ] as const) {
    if (secs % size === 0) {
      return `${secs / size}${unit}`;
    }
  }
  return `${secs}s`;
}

export const idlePattern = /^[1-9]\d*[smhd]$/;

export function toValues(info: WorkspaceInfo): MachineValues {
  return {
    vcpus: info.vcpus,
    memoryGib: info.memory_mib / 1024,
    root: info.root,
    workspaceDiskGib: info.workspace_disk_gib,
    dockerDiskGib: info.docker_disk_gib,
    idle: info.idle_timeout_secs == null ? 'never' : 'after',
    idleAfter: info.idle_timeout_secs == null ? '30m' : formatIdle(info.idle_timeout_secs),
  };
}

// 変えた項目だけを送る。
export function toPatch(initial: MachineValues, values: MachineValues): UpdateWorkspace {
  const patch: UpdateWorkspace = {};
  if (values.vcpus !== initial.vcpus) patch.vcpus = values.vcpus;
  if (values.memoryGib !== initial.memoryGib) {
    patch.memory_mib = Math.round(values.memoryGib * 1024);
  }
  if (values.root !== initial.root) patch.root = values.root;
  if (values.workspaceDiskGib !== initial.workspaceDiskGib) {
    patch.workspace_disk_gib = values.workspaceDiskGib;
  }
  if (values.dockerDiskGib !== initial.dockerDiskGib) patch.docker_disk_gib = values.dockerDiskGib;
  const idleChanged =
    values.idle !== initial.idle ||
    (values.idle === 'after' && values.idleAfter !== initial.idleAfter);
  if (idleChanged) {
    patch.idle_timeout = values.idle === 'after' ? values.idleAfter : values.idle;
  }
  return patch;
}

// 動いている VM には効かず、次の起動から効く項目を変えるか。
export function needsRestart(patch: UpdateWorkspace): boolean {
  return (
    patch.vcpus != null ||
    patch.memory_mib != null ||
    patch.root != null ||
    patch.workspace_disk_gib != null ||
    patch.docker_disk_gib != null
  );
}
