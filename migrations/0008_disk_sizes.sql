-- ワークスペースのディスクの大きさ (GiB)。NULL ならゲートウェイの設定の既定を使う。
-- 大きくすることしかできず、次にランナーを起動したときに広げる。
ALTER TABLE workspaces ADD COLUMN workspace_disk_gib INTEGER;
ALTER TABLE workspaces ADD COLUMN docker_disk_gib INTEGER;
