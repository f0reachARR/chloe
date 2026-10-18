-- VM の vCPU の数とメモリ (MiB)。NULL ならゲートウェイの設定の既定を使う。次の起動から効く。
ALTER TABLE workspaces ADD COLUMN vcpus INTEGER;
ALTER TABLE workspaces ADD COLUMN memory_mib INTEGER;
-- 最後の接続が閉じてから止めるまでの秒数。NULL ならゲートウェイの idle_timeout_secs、
-- 0 なら止めない。
ALTER TABLE workspaces ADD COLUMN idle_timeout_secs INTEGER;
