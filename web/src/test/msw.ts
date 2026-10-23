// 偽の管理 API。手続きは web/openapi.json から作った型に合わせて書くので、スペックと食い違えば
// 型の検査で落ちる。
import { setupServer } from 'msw/node';
import { createOpenApiHttp } from 'openapi-msw';

import type { paths } from '@/shared/api/schema';

export const http = createOpenApiHttp<paths>({ baseUrl: window.location.origin });

export const server = setupServer();
