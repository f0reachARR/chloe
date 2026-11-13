# Web UI guidelines

This document lists the rules to follow when building the web UI (`web/`), and the reasons behind them.
How these decisions were made is in stages 20 to 22 of the [implementation log](design/implementation-steps.md) (Japanese); the development workflow (commands, adding a feature) is in [Development](development.md#web-ui).

## What the UI is for

The web UI is the entry point for people to drive the agents inside the VMs.
It lets you answer asks from agents, see whether an agent is busy, and use the VM's shell, without an SSH terminal.
Agents run unattended and asks can arrive at any time, so being able to answer from the smartphone in your hand comes first.

The UI does not replace the `chloe` command over SSH.
It can only do what the admin API can do, and its permissions are the same as over SSH.
Do not add operations that exist only in the UI, or permissions that apply only to the UI.

## Technology choices

| Part | Choice | Reason |
| --- | --- | --- |
| UI | React SPA | We add stateful screens such as the web terminal and the rule editor |
| Terminal | xterm.js (`@xterm/xterm`, `@xterm/addon-fit`) | Used by VS Code and others, with complete terminal features. Only included in the terminal screen's chunk |
| Components | Mantine (`@mantine/form` for form state) | A complete component set that fits dark color schemes and smartphone widths out of the box |
| Router | TanStack Router (file-based routes) | Routes and search params are typed, and it pairs with TanStack Query |
| Server state | TanStack Query | Refetching, caching, and event-driven invalidation in one place |
| API calls | openapi-fetch, openapi-react-query | Types are generated from the OpenAPI spec, not kept in sync by hand |
| Build | Vite, pnpm | |
| PWA | vite-plugin-pwa (Service Worker written by hand) | Can be opened from the home screen, and push notifications can be added later |
| Lint and format | oxlint, oxfmt | Fast, with little configuration (similar to clippy and rustfmt on the Rust side) |
| Tests | Vitest, Testing Library, MSW (openapi-msw) | Fake APIs can be written against the spec's types |

To change any of these choices, first write it up as a stage in the docs and discuss it before changing.

## Layout and dependency direction

```text
web/src/
  app/              bootstrap, router, feature list (features.ts), shell (header and navigation)
  routes/           route files (routeTree.gen.ts is generated from these)
  features/<feature>/
    index.ts        entry point. Exports only what others may use
    feature.ts      declares navigation items and event handlers
    api.ts          this feature's queries (queryOptions) and refetching
    *.tsx           screens and components (*.test.tsx are their tests)
  shared/           API client and types, event dispatcher, cross-feature components and functions
  test/             test setup (MSW fake API, rendering, data)
```

Dependencies may only point from `app` and `routes` → feature entry points (`@/features/<feature>`) → `shared`.

- **Features do not use each other**: move anything two features need into `shared`. This way, adding or removing a feature does not ripple into other features.
- **`shared` does not know about features**: if `shared` used a feature, removing that feature would break `shared`.
- **Features are used through their entry point**: you can rearrange files inside a feature without changing its users.

`pnpm lint` (`no-restricted-imports` in `.oxlintrc.json`) enforces this direction.
Fix where the code lives instead of loosening the rule to make it pass.

## Building a feature

- **Features declare themselves**: navigation items (and, if they show a count, the component that renders it) and per-event handlers go in the feature's `feature.ts`; the shell collects and assembles them. Do not edit the shell's internals to add a feature.
- **Keep route files thin**: they only validate search params (zod), load data (fill the TanStack Query cache in `loader`), and render the feature's screen. Screen components take search params as arguments so they can be tested without the router.
- **Where state lives**: anything from the server lives only in TanStack Query. Things that should survive reopening the page, like filters and view toggles, live in URL search params. Things local to one screen live in `useState`. Do not add a global store until something does not fit any of these.
- **Do not block loading**: slow queries that go to the VM (checking whether an agent is busy) are not awaited in the route loader; the screen shows a loading state instead.
- **Screens that need login**: put them under `routes/_app/`. `_app`'s `beforeLoad` checks the session and redirects to the login screen if there is none.
- **Heavy libraries only on their screen**: load large libraries like xterm.js only inside the feature that uses them. Code is split per route, so they stay out of other screens' chunks (check the `pnpm build` output).
- **What routes declare to the shell**: routes that draw at full screen width (the terminal) set `staticData: { wide: true }`. The shell fits other screens into a readable width.
- **Buttons inside forms**: set `type="button"` on an `ActionIcon` placed inside a `<form>`. Otherwise it becomes a submit button, and pressing Enter in a field triggers the reorder or delete button.
- **Overlays**: things you want to open on top of any screen, like workspace settings and asks, are not routes; they open through `_app` search params (`shared/overlays.ts`, `?settings=`, `?asks=`) and the shell renders them. Opening and closing does not change the route, so the screen underneath (the terminal) is not recreated and the shell session continues. The opener only places a link that rewrites the search params with `openOverlay`, so it does not need to use another feature. Features declare the rendering component in `overlays` (settings alone is rendered by the shell, because it collects sections from every feature). To add one, add its name to `overlaySearchSchema`. Closing replaces the history entry so that Back does not reopen it.
- **Add workspace settings as sections**: to add a setting, the feature declares a section (`id`, name, icon, order, component) in `workspaceSettings`; do not edit the contents of the shell's modal. Each section saves to a different API, so a section loads its own data, saves with its own save button, and reports unsaved changes through `onDirtyChange` (the shell checks this on close). Form state is held in `@mantine/form`, and only changed fields are sent. The current sections are Machine, Ports, Delete (`features/workspaces`), and Policy (`features/policies`). Delete has a large order value so it comes last.
- **Choosing an owner**: fields that choose among a user, organization, or team, such as where to create a workspace or who owns a secret, use the query in `shared/owners.ts` (`/api/v1/owners`) and `shared/ui/OwnerSelect`. Owners are written `user:alice`, `org:acme`, `team:acme/dev`.
- **Placing another feature's components in a header and similar**: when a feature's screen hosts another feature's component, like the asks button in the terminal header, the screen exposes a slot (`actions` and so on) and the route file assembles them. Routes may use the entry points of several features.

## Relationship to the admin API

The admin API's types originate in the Rust code.
The Rust side generates the spec from `utoipa` annotations and writes it to `web/openapi.json`; the UI's types are generated from that.
Do not hand-write API types on the UI side.
When you change the API, regenerate the spec and the types (`cargo test` fails if the spec is out of sync with the code).

Every path the gateway serves is under `/api`; all other paths belong to the UI.
Do not create UI routes whose paths start with `/api`.

The workspace rule types (`WorkspacePolicy`, `PathRuleConfig`) are exposed in the spec.
So are user, organization, and team settings (`PolicyLayer`, `GitHubScope`), and the bodies for defining and registering secrets (`SecretSpec`, `SecretInput`).

APIs that are read and then replaced (workspace rules; user, organization, and team settings) send the version they read in `If-Match`. While you are editing in the UI, someone else's save or an approved ask can change the data, so on a 412 the UI keeps your edits and asks whether to load the latest or overwrite (`useVersionedSave` in `features/policies`).

## Events

The shell receives events from the gateway (SSE on `/api/v1/events`) over a single connection and passes each event, by name, to the handlers that features declared.
Browsers limit the number of connections to the same server, so do not open a connection per feature.

- An event only says "something changed"; the UI refetches its queries to get the latest state. Do not accumulate event payloads as UI state. This way, if an event is missed, refetching restores the correct state.
- `resync` is called on connect and when the server reports missed events (`lagged`). Features refetch their own queries without looking at events.
- For a new kind of event, add its name on the gateway side (currently `ask`), and the feature that receives it writes a handler in `feature.ts`.

## Security

- **Render text from the VM as strings**: an ask's destination, question, choices, and reason are written by processes inside the VM. Render them as React strings; never pass them through `dangerouslySetInnerHTML` or Markdown-to-HTML conversion.
- **Do not load external resources**: scripts, fonts, and images come only from the UI's own files. The gateway returns a CSP with `script-src 'self'` (inline is allowed only for styles, because Mantine injects CSS variables).
- **Do not create long-lived credentials from the UI**: the gateway refuses to create API tokens over a cookie session. Even if the UI has an XSS, an attacker cannot turn an expiring session into a non-expiring credential. When adding operations of the same nature (registering SSH keys and so on), keep them unavailable from the UI as well.
- **Secrets can only be changed by re-entering their values**: secrets can be registered from the UI. Values cannot be read back, and changing a definition always requires sending every value (`keep_secret_values`, which changes only the definition while keeping registered values, is used only by the TUI). Even with an XSS in the UI, an attacker can replace a value with one they know, but cannot redirect a registered real value to another destination. To keep this boundary, do not add a path in the API or the UI that changes a definition while keeping values. In the UI, clear the value fields after submitting, and do not keep the submitted contents in TanStack Query state (`gcTime: 0` on `useMutation`, and `reset` once it finishes).
- **Cross-site requests**: for non-GET requests and WebSocket upgrades that arrive with a cookie, the gateway checks `Origin` and refuses them. Do not build anything on the UI side that sends cookies to another origin.
- **Do not put pages from the VM on the UI's origin**: agents can rewrite the pages of published ports. If they were on the same origin as the UI, their scripts could call the admin API with the user's cookie, so the gateway proxies each port on a separate host (`[api] proxy_url`). The UI only shows links that open published ports in a separate tab (`rel="noopener noreferrer"`), and never embeds them in a frame (iframe). The proxy hosts are all on the same site, so the gateway checks `Origin` and `Sec-Fetch-Site` on cookie requests and refuses those sent from another host's page.
- **Recheck long-lived connections**: for connections where closing loses work, like the terminal, the gateway rechecks the session while they are open (every minute) instead of forcing a reconnect. Do the same when adding connections of the same nature.

## Look and feel

- **Check smartphone width first**: confirm the UI is usable at about 390px wide before polishing wide screens.
- **Navigation**: at the bottom on smartphone widths, on the left on wide screens. The hidden one is not rendered (so focus does not move to off-screen links).
- **Keep the look consistent with the Mantine theme**: colors, spacing, and corner radii are set in `app/theme.ts` and component props. Write your own CSS only when a Mantine component cannot express it (such as the bottom navigation), and use CSS Modules for it.
- **Color scheme**: switch between light and dark following the device setting. Check that both are readable.
- **Wording**: write in English. Keep it consistent with the wording of the `chloe` command and the admin TUI.

## Distribution

- The built UI is not embedded in the binary; it ships in the deb and is served from files (`/usr/share/chloe/web`, `[api] web_root`).
- Files Vite emits under `/assets/` have a content hash in their names, so they are cached for a long time; everything else (`index.html`, the Service Worker) is revalidated every time. Files not present under `/assets/` return 404.
- The Service Worker precaches only the UI's files, and never caches `/api` responses.

## Tests

- **Components and screens**: tested with Vitest and Testing Library against an MSW fake API. Fake API handlers are written with `openapi-msw` to match the spec's types.
- **Mechanisms without a screen, like event dispatch**: tested by passing a fake EventSource and the like.
- **Tests against the real gateway**: API rules (cookies, `Origin`, permissions) are checked by `mvp-test.sh` (curl with cookies and a Node WebSocket), and UI behavior by Playwright in `web/e2e` (`web-test.sh`). E2E covers the paths users take (login, terminal, answering asks) at both smartphone width and on a wide screen. Fine-grained cases belong in Vitest.

## Open questions

- **Push notifications**: these need a Service Worker and VAPID keys. When added, they go in `web/src/sw.ts`.
- **Localization**: English only for now; no mechanism is in place.
- **Terminal reconnection**: after a disconnect you cannot reconnect to the same shell (same as SSH). This needs a mechanism on the gateway side to keep the shell alive.
