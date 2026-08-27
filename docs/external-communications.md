# External communications

ClawOS can expose the same system agent through an external chat bot. Discord
and Telegram support bidirectional conversations. The other gateway apps are
currently outbound integrations unless their own documentation says otherwise.

| Platform | Inbound to agent | Outbound | Transport |
| --- | --- | --- | --- |
| Discord | Yes | Yes | Gateway v10 WebSocket + REST |
| Telegram | Yes | Yes | Bot API long polling |
| Slack, Teams, Matrix, Signal, WhatsApp, and other gateway apps | Not yet | Yes | Provider-specific API or webhook |

External messages can drive a system-level agent, so inbound access always
fails closed. Do not use a wildcard user allowlist on an Internet-reachable
bot.

## Discord

### 1. Create the bot

1. Create an application in the
   [Discord Developer Portal](https://discord.com/developers/applications),
   then add a bot.
2. On the **Bot** page, enable **Message Content Intent**. The gateway does not
   request Server Members Intent.
3. Use OAuth2 URL Generator with the `bot` scope. Grant **View Channels**,
   **Send Messages**, **Read Message History**, and **Send Messages in
   Threads**, then invite the bot to the server.
4. Enable Discord Developer Mode and copy the user, server, and optional
   channel IDs that should be allowed.

### 2. Store the token and access policy

Store the raw bot token in the ClawOS credential store:

```bash
cos credential store discord_bot_token "<bot-token>"
```

Persist a fail-closed policy. `users` is mandatory. Guild messages also need
either a matching `guilds` or `channels` entry:

```bash
cos app gateway-discord configure \
  "users=123456789012345678 guilds=987654321098765432 require_mention=true rpm=5"
```

Multiple IDs are comma-separated:

```bash
cos app gateway-discord configure \
  "users=111111111111111111,222222222222222222 channels=333333333333333333,444444444444444444"
```

The command updates only the supplied fields. Available settings are:

| Setting | Default | Meaning |
| --- | --- | --- |
| `users` | none | Discord user IDs allowed to invoke the agent; required |
| `guilds` | none | Server IDs allowed for guild messages |
| `channels` | none | Channel or thread IDs allowed for guild messages |
| `require_mention` | `true` | Require an `@bot` mention in guild channels |
| `rpm` | `5` | Agent requests per user per minute, from 1 through 60 |

DMs require only an allowed user. Guild channels and threads keep separate
conversation sessions, while each user's DMs keep their own session. Messages
from bots are ignored, and generated replies cannot trigger `@everyone`, role,
or user mentions.

Allowlisting a guild covers its threads. If policy is scoped only with
`channels`, add each thread ID explicitly; a parent channel ID does not
implicitly allow its child threads.

Environment variables override persisted settings for service deployments:

```text
COS_DISCORD_TOKEN
COS_DISCORD_ALLOWED_USERS
COS_DISCORD_ALLOWED_GUILDS
COS_DISCORD_ALLOWED_CHANNELS
COS_DISCORD_REQUIRE_MENTION
COS_DISCORD_RPM
```

Prefer the credential store over `COS_DISCORD_TOKEN`.

### 3. Run it

Start in the foreground while testing:

```bash
cos app gateway-discord start
```

Check or stop it from another terminal:

```bash
cos app gateway-discord status
cos app gateway-discord stop
```

Register it with the ClawOS service manager for a persistent deployment:

```bash
cos service register \
  --name discord-agent \
  --description "ClawOS Discord agent gateway" \
  --command "cos app gateway-discord start"
cos service start discord-agent
cos service logs discord-agent --tail 50
```

The gateway reconnects with exponential backoff, resumes valid Discord
sessions, keeps WebSocket heartbeats independent from agent response time, and
splits long agent responses at Discord's 2,000-character limit.

### Troubleshooting

- **Close code 4014:** enable Message Content Intent in the Developer Portal.
- **DM is ignored:** add the sender's numeric user ID to `users`.
- **Server message is ignored:** allow its guild or channel and mention the
  bot unless `require_mention=false`.
- **Agent answers do not remember earlier messages:** inspect
  `cos app gateway-discord status`; `agent_sessions` should increase after
  successful conversations.
- **Permission denied:** grant the `net.dial`, `secret.read`,
  `data.inbox.write`, `agent.spawn`, process, and memory capabilities declared
  by `gateway-discord`.

## Telegram

Telegram already supports bidirectional long polling:

```bash
cos credential store telegram_bot_token "<bot-token>"
export COS_TELEGRAM_ALLOWED_CHATS=123456789
cos app gateway-telegram start
```

`COS_TELEGRAM_ALLOWED_CHATS` is mandatory and accepts comma-separated chat
IDs. `COS_TELEGRAM_RPM` changes the default five requests per minute.

## Outbound-only gateways

Outbound gateways use the same app surface:

```bash
cos app gateway-slack send C123ABC "deployment finished"
cos app gateway-matrix send '!room:example.org' "deployment finished"
cos app gateway-ntfy send "deployment finished" --topic alerts
```

Run `cos app <gateway-id> --schema` to inspect the exact credentials,
arguments, and operations supported by an installed gateway.
