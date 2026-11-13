# GitHub App setup

chloe uses a single GitHub App for three purposes.

- **Device Flow**: authenticates users via SSH `keyboard-interactive` and obtains GitHub credentials (a user token) for their personal workspaces.
- **Membership checks**: uses an installation token to check whether a user belongs to an organization or team.
- **GitHub credentials for shared workspaces**: issues installation tokens scoped to specific repositories and permissions per workspace.

## Where to register

Register the app under the organization (the organization's Settings → Developer settings → GitHub Apps → New GitHub App).
If you register it under a personal account, installing it in an organization requires approval on the organization's side.

## Registration form settings

| Field | Setting |
| --- | --- |
| GitHub App name, Homepage URL | Anything |
| Callback URL | If you log in to the web UI with GitHub, `<public_url>/api/auth/github/callback` (for example `https://chloe.example.com/api/auth/github/callback`). Otherwise leave it empty |
| Expire user authorization tokens | Enabled (user tokens expire after 8 hours and are renewed with a refresh token) |
| Request user authorization (OAuth) during installation | Disabled |
| Enable Device Flow | Enabled |
| Webhook Active | Disabled (chloe does not receive webhooks) |
| Where can this GitHub App be installed? | Only on this account (to use it with several organizations, see [Multiple organizations](#multiple-organizations) below) |

## Permissions

Start with the following permissions and add any you are missing later.
When you change the app's permissions, each organization it is installed in must approve them again.

| Type | Permission | Setting | Purpose |
| --- | --- | --- | --- |
| Repository | Contents | Read and write | clone, fetch, push |
| Repository | Metadata | Read-only | Required (added automatically) |
| Repository | Pull requests | Read and write | Creating and reviewing pull requests |
| Repository | Issues | Read and write | Reading and commenting on issues |
| Repository | Actions | Read-only | Reading CI results and logs |
| Repository | Checks | Read-only | Reading CI results |
| Repository | Commit statuses | Read-only | Reading CI results |
| Repository | Workflows | No access | Disallows changes to `.github/workflows` |
| Organization | Members | Read-only | Checking organization and team membership |

With Workflows set to No access, GitHub rejects pushes of commits that change anything under `.github/workflows`.
Start from this setting as a default that keeps agents from rewriting CI definitions.

A user token's permissions are the intersection of the app's permissions and the user's own permissions.
For shared workspaces, the organization's administrators further narrow the installation token per workspace, within the permissions granted here.

## Information to note after registration

| Item | Where to find it | Secret? |
| --- | --- | --- |
| App ID | About on the app's settings page | Not secret |
| Client ID | About on the app's settings page | Not secret |
| Private key (`.pem`) | Private keys → Generate a private key on the app's settings page | **Secret** |
| Organization name the app is installed in | The organization where you installed the app | Not secret |

For SSH login (Device Flow) alone, you don't need a client secret.
User tokens obtained through Device Flow can be renewed without a client secret.
Only if you log in to the web UI with GitHub (the OAuth web flow), create one under Client secrets → Generate a new client secret on the app's settings page, and treat it as a **secret**, just like the private key.
Even without a client secret, you can log in to the web UI with the URL that `chloe web login` prints over SSH.

Install the app in the organization and select the repositories chloe should work with (Only select repositories).
You don't need to note the installation ID: chloe looks it up through the API using the app's private key.

## Where to store the credentials

Don't paste the private key into chats or repositories; make it readable only by the user that runs the gateway.

### On a host

With the layout from [Installation](install.md), put the files in `/etc/chloe/` and make them readable by the group of `chloe`, the user that runs the gateway.

```sh
sudo install -m 640 -g chloe ~/Downloads/<app-name>.*.private-key.pem /etc/chloe/github-app.pem
# Only if you log in to the web UI with GitHub
sudo install -m 640 -g chloe /dev/stdin /etc/chloe/github-app-client-secret <<< '<client secret>'
```

`/etc/chloe/github-app.toml`:

```toml
app_id = 123456
client_id = "Iv23liXXXXXXXXXXXXXX"
private_key = "file:/etc/chloe/github-app.pem"
# Only if you log in to the web UI with GitHub.
# client_secret = "file:/etc/chloe/github-app-client-secret"
# Set this to restrict which organizations are used. If omitted, every
# organization the app is installed in is used.
# organizations = ["ForteFibre"]
```

`private_key` and `client_secret` are read as secret references.
Once you move to 1Password Connect, rewrite them as references of the form `op://<vault>/<item>/<field>` and delete the private key file.

Put this path in the gateway configuration (`/etc/chloe/gateway.toml`).

```toml
github_app = "/etc/chloe/github-app.toml"
```

### For development

Keep the files outside the repository, in `~/.config/chloe/`.

```sh
mkdir -p ~/.config/chloe
chmod 700 ~/.config/chloe
mv ~/Downloads/<app-name>.*.private-key.pem ~/.config/chloe/github-app.pem
chmod 600 ~/.config/chloe/github-app.pem
```

Write `~/.config/chloe/github-app.toml` the same way as above, with `private_key` set to `file:/home/<user>/.config/chloe/github-app.pem`.
In the gateway configuration, write `github_app = "~/.config/chloe/github-app.toml"` (`~` expands to the home directory of the user that started the gateway).

## How the gateway uses it

When the gateway configuration's `github_app` points to the GitHub App configuration file, the following applies.

- Members of any organization the app is installed in can log in. `allowed_users` lists additional users allowed on top of them.
- An organization's workspaces (`org/name`) are open to its members, and a team's workspaces (`org/team/name`) to that team's members. Membership can be checked only for organizations the app is installed in.
- To log in over SSH, complete Device Flow once via keyboard-interactive. Even with a public key registered on GitHub, you proceed to Device Flow if you have no user token.
- With `ssh_user` (`chloe` by default) or a user name that doesn't exist on GitHub, the account approved in Device Flow becomes the user, and the public key used at that time is linked to it. From then on, you can log in with that key.
- Personal workspaces use the user token obtained through Device Flow. To obtain a new token, run `ssh <host> chloe github login`. The token is stored encrypted in chloe's database; when the runner's proxy asks for it at injection time and fewer than 5 minutes remain before it expires, chloe renews it before answering. Only placeholders go into the VM.
- Team and organization workspaces get a token for the app's installation in that organization. You can narrow its scope with `[github]` in the organization and team settings (`chloe policy set`).
- Users who are organization administrators on GitHub (the `admin` role) can change that organization's and its teams' settings and secrets, and the workspace rules. Team maintainers can change that team's secrets and workspace rules.

`git` inside the VM connects to GitHub over HTTPS, and a credential helper returns the placeholder (SSH remotes are rewritten to HTTPS too).

You can check the configuration with `cargo run -p chloe-github --example check -- ~/.config/chloe/github-app.toml <login>` (token values are not printed).

## Multiple organizations

chloe uses every organization the app is installed in (unless restricted by `organizations` in the App configuration).
If you register the app as Only on this account, it can be installed only in the organization that registered it.
To use it in other organizations, change the app to Any account and install it in those organizations.

With Any account, anyone can install the app in their own organization, and its members can then log in to chloe and use VMs.
In that case, restrict this by listing the allowed organizations in `organizations` in the App configuration.
