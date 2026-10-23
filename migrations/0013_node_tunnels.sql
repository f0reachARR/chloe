-- ホストからコントロールプレーンへ張るトンネル (段階 22)。コントロールプレーンはホストに
-- つながないので、ホストの URL は要らない。
ALTER TABLE nodes DROP COLUMN url;

-- トンネルがつながった時刻。切れたら消す。ゲートウェイの起動のときにすべて消す。
ALTER TABLE nodes ADD COLUMN connected_at INTEGER;
