import { useNavigate, useSearch } from '@tanstack/react-router';

import { closeOverlays } from '@/shared/overlays';

import { overlays, workspaceSettings } from '../features';
import { WorkspaceSettings } from '../settings/WorkspaceSettings';

// 検索パラメーターで開いている、重ねて描くもの (設定と、機能が申告したもの)。閉じるときは履歴を
// 置き換える (戻るで開き直さないように)。
export function Overlays() {
  const search = useSearch({ from: '/_app' });
  const navigate = useNavigate();
  const close = () => void navigate({ to: '.', search: closeOverlays, replace: true });
  return (
    <>
      {search.settings && (
        <WorkspaceSettings
          key={search.settings}
          workspace={search.settings}
          sections={workspaceSettings}
          section={search.section}
          onSectionChange={(section) =>
            void navigate({ to: '.', search: (prev) => ({ ...prev, section }), replace: true })
          }
          onClose={close}
        />
      )}
      {overlays.map(({ param, component: Overlay }) => {
        const workspace = search[param];
        return workspace ? (
          <Overlay key={`${param}:${workspace}`} workspace={workspace} onClose={close} />
        ) : null;
      })}
    </>
  );
}
