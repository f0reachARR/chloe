/// <reference lib="webworker" />
// 画面の Service Worker。画面の殻 (ビルドしたファイル) だけを前もってキャッシュし、`/api` の応答は
// キャッシュしない。あとでプッシュ通知を足すときは、ここに push と notificationclick を足す。
import { clientsClaim } from 'workbox-core';
import { createHandlerBoundToURL, precacheAndRoute } from 'workbox-precaching';
import { NavigationRoute, registerRoute } from 'workbox-routing';

declare let self: ServiceWorkerGlobalScope;

// oxlint-disable-next-line no-underscore-dangle -- Workbox が埋め込む名前
precacheAndRoute(self.__WB_MANIFEST);

// 画面のパスは index.html で開く。`/api` (ログインの送り出しやコールバックを含む) はゲートウェイに任せる。
registerRoute(
  new NavigationRoute(createHandlerBoundToURL('/index.html'), {
    denylist: [/^\/api\//],
  }),
);

self.skipWaiting();
clientsClaim();
