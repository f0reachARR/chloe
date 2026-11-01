// 画面の上に重ねて開くもの (ワークスペースの設定、依頼)。開いていることは `_app` の検索パラメーターに
// 置くので、開き閉じでルートは変わらず、下の画面 (端末など) は動き続ける。
import { z } from 'zod';

export const overlaySearchSchema = z.object({
  // 設定を開くワークスペースと、開くセクション。
  settings: z.string().min(1).optional(),
  section: z.string().min(1).optional(),
  // 依頼を開くワークスペース。
  asks: z.string().min(1).optional(),
});

export type OverlaySearch = z.infer<typeof overlaySearchSchema>;

export type WorkspaceOverlay = 'settings' | 'asks';

const closed: OverlaySearch = { settings: undefined, section: undefined, asks: undefined };

// 検索パラメーターを書き換えて重ねるものを開く (Link や navigate の `search` に渡す)。一度に一つだけ開く。
export function openOverlay(overlay: WorkspaceOverlay, workspace: string, section?: string) {
  return <T extends object>(prev: T): T & OverlaySearch => ({
    ...prev,
    ...closed,
    [overlay]: workspace,
    ...(overlay === 'settings' && section ? { section } : {}),
  });
}

export function closeOverlays<T extends object>(prev: T): T & OverlaySearch {
  return { ...prev, ...closed };
}
