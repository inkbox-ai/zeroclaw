# A2A agent discovery

This deployment can publish its agents so another deployment, or any HTTP
client, can find them and call them. A2A is the protocol for one agent to reach
another agent, the way a person reaches a bot over a chat app. This page shows
exactly what to type and exactly what comes back.

Every response on this page is real output from a running daemon. Nothing here
is illustrative.

## The whole thing in two requests

You only ever need two GET requests to discover an agent.

First, ask the deployment which agents it publishes:

```
curl http://localhost:42617/.well-known/agents-card.json
```

Second, ask one of those agents what it can do:

```
curl http://localhost:42617/a2a/translator/.well-known/agent-card.json
```

The first request gives you a list of agent URLs. The second gives you one
agent's skills and the URL you send work to. That is the entire discovery
surface. The rest of this page is just reading those two responses carefully.

## Request 1: list the agents

```
curl http://localhost:42617/.well-known/agents-card.json
```

Response:

```json
{
    "name": "ZeroClaw agents",
    "description": "Discovery catalog enumerating published A2A agents on this ZeroClaw install. Not a runnable agent; each entry below serves its own A2A card and endpoint. Skills are aggregated from the published agents, each tagged with its owning alias.",
    "supportedInterfaces": [
        {
            "url": "http://localhost:42617/.well-known/agents-card.json",
            "protocolBinding": "catalog",
            "protocolVersion": "1.0"
        },
        {
            "url": "http://localhost:42617/a2a/invoicer",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        },
        {
            "url": "http://localhost:42617/a2a/translator",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }
    ],
    "version": "0.8.0",
    "capabilities": {
        "streaming": false,
        "pushNotifications": false,
        "extendedAgentCard": false
    },
    "defaultInputModes": ["text"],
    "defaultOutputModes": ["text"],
    "skills": [
        {
            "id": "translator/translate",
            "name": "Translate",
            "description": "Translate text between languages.",
            "tags": ["translator"]
        },
        {
            "id": "invoicer/draft-invoice",
            "name": "Draft Invoice",
            "description": "Draft an invoice line item from a description and amount.",
            "tags": ["invoicer"]
        }
    ]
}
```

Read it like this. `supportedInterfaces` lists URLs. The one tagged `catalog` is
this list itself, ignore it. The two tagged `JSONRPC` are the agents:
`translator` and `invoicer`. Their URLs are where you will send work. `skills`
aggregates every published agent's skills, each `id` prefixed and `tags`-tagged
with the owning alias, so one read shows the whole install's capability surface
and who owns each piece.

## Request 2: inspect one agent

Take a URL from the list and append the card path:

```
curl http://localhost:42617/a2a/translator/.well-known/agent-card.json
```

Response:

```json
{
    "name": "translator",
    "description": "ZeroClaw agent 'translator'.",
    "supportedInterfaces": [
        {
            "url": "http://localhost:42617/a2a/translator",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }
    ],
    "version": "0.8.0",
    "capabilities": {
        "streaming": false,
        "pushNotifications": false,
        "extendedAgentCard": false
    },
    "defaultInputModes": ["text"],
    "defaultOutputModes": ["text"],
    "skills": [
        {
            "id": "translate-text",
            "name": "Translate Text",
            "description": "Translate a short text between English and Spanish.",
        },
        {
            "id": "detect-language",
            "name": "Detect Language",
            "description": "Detect the language of a given snippet of text.",
        }
    ]
}
```

Now you know three things. The agent is named `translator`. It has two skills,
`translate-text` and `detect-language`, with plain descriptions of what each
does. And the single `JSONRPC` interface URL, `http://localhost:42617/a2a/translator`,
is the address you POST a task to.

## What an agent chooses to show

An agent does not have to publish every skill it has. The `invoicer` agent in
this same deployment has two skills on disk but publishes only one:

```
curl http://localhost:42617/a2a/invoicer/.well-known/agent-card.json
```

```json
{
    "name": "invoicer",
    "description": "ZeroClaw agent 'invoicer'.",
    "supportedInterfaces": [
        {
            "url": "http://localhost:42617/a2a/invoicer",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }
    ],
    "version": "0.8.0",
    "capabilities": {
        "streaming": false,
        "pushNotifications": false,
        "extendedAgentCard": false
    },
    "defaultInputModes": ["text"],
    "defaultOutputModes": ["text"],
    "skills": [
        {
            "id": "draft-invoice",
            "name": "Draft Invoice",
            "description": "Draft an invoice line item from a description and amount.",
        }
    ]
}
```

It has a `tax-estimate` skill too. It is not on the card. The deployment chose to
expose only `draft-invoice`. That is the whole point of publishing: you decide
per agent which skills the outside world can see.

## Sending a task

Once you have an agent's interface URL and a skill, you send work as a JSON-RPC
`message/send` POST to that URL:

```
curl -X POST http://localhost:42617/a2a/translator \
  -H 'Content-Type: application/json' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "message/send",
    "params": {
      "message": {
        "role": "user",
        "parts": [{ "kind": "text", "text": "Translate to Spanish: good morning" }]
      }
    }
  }'
```

The agent runs the turn and answers with a completed task. The reply is the text
part inside the task's artifact:

```
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "id": "ab381abc-f744-4bb4-8635-e0d6f9f43592",
    "contextId": "a2a_translator_ce7318d7-d687-4581-9d8b-4facd5527aec",
    "kind": "task",
    "status": { "state": "completed" },
    "artifacts": [
      {
        "artifactId": "44978c13-7fd4-4e35-934f-edc9f175b9dd",
        "parts": [{ "kind": "text", "text": "buenos días" }]
      }
    ]
  }
}
```

The interface URL is the same for discovery and for tasks; only the request
changes. The endpoint accepts only `message/send`; any other `method` returns a
JSON-RPC `-32601`, an empty message returns `-32602`, and a body that is not
JSON-RPC returns HTTP `400`.

## Exposure and the one sharp edge

The task endpoint shares the exact posture of the cards: it answers only when
`[a2a.server] enabled` is set and the alias is enabled and published. A task POST
to an unpublished or unknown alias returns `404`, the same as its card.

One sharp edge to know about: the interface URL answers a bare GET with the web
dashboard, not an agent, because the gateway falls back to serving the dashboard
for any path it does not recognize:

```
curl -i http://localhost:42617/a2a/translator
HTTP/1.1 200 OK
content-type: text/html
```

Discovery (the `.well-known` paths) and the `message/send` POST are the supported
surface. A bare GET on the interface URL is not part of the protocol; read the
card at the `.well-known` path instead.

## Where the agents serve from

The cards are served by the web gateway, on the same address and port as
everything else. If your gateway is on `localhost:42617`, that is where the
catalog and every agent card live. You do not run a second server and you do not
open a second port.

If you put this deployment behind a reverse proxy or a public hostname, the URLs
inside the cards need to match the address clients actually reach. The published
URL is resolved in this order: an explicit public base URL if you set one, then
an A2A-specific host and port override if you set those, then the gateway's own
address. The override exists for the proxy case; if you are not behind a proxy
you never touch it and the cards advertise the gateway address directly.

## Turning it on

Discovery is off until you turn it on, and it is off in three independent ways
so nothing leaks by accident:

- The A2A server is disabled for the whole deployment by default.
- Each agent is unpublished by default, even with the server on.
- A published agent exposes only the skills you name, nothing more.

You enable the server once, mark the specific agents you want reachable as
published, and list the skills each one exposes. An agent that is disabled, or
not published, does not appear in the catalog and its card path returns `404`. An
unknown agent name returns `404` as well.

A named skill appears on the card only when it resolves to a real skill the agent
actually carries: it must live in one of the agent's skill bundles and its
`SKILL.md` must have valid YAML frontmatter. A name that does not resolve, or a
skill in a bundle the agent does not declare, is dropped silently rather than
advertised.

## How several deployments connect

Discovery composes across any number of deployments. Each deployment publishes
its own catalog at its own address. A client that knows several deployment
addresses fetches each catalog, reads the agents, and now holds a combined map of
every reachable agent across all of them. There is no registry and no central
server: the client is the only thing that needs to know the addresses, and it
talks to each deployment directly.

A worked picture. You run a personal deployment. Your team runs a shared one. A
data team runs a third. Your client fetches all three catalogs:

```
curl http://personal.example:42617/.well-known/agents-card.json
curl http://team.example:42617/.well-known/agents-card.json
curl http://data.example:42617/.well-known/agents-card.json
```

Each returns its own agent list. Your client now sees, say, a `notes` agent at
personal, a `deploy` agent at team, and a `query` agent at data. To use any of
them it fetches that agent's card and sends a task to that agent's URL, exactly
as shown above. Nothing changes per deployment; it is the same two reads and one
POST, pointed at a different host.

## Use cases

A few concrete reasons to wire deployments together.

A research deployment hands literature search to a specialist data deployment.
The research agent discovers the data deployment's `search` agent, sends it a
query as a task, and folds the result into its own work. The research side never
holds the data side's credentials or indexes; it only knows the agent URL.

An on-call deployment fans an incident out to team-owned deployments. It
discovers a `triage` agent in each team's deployment and sends each one the same
incident as a task, collecting their answers. Each team controls what their
triage agent exposes; the on-call side just reads cards and sends tasks.

A personal deployment calls a company deployment's vetted agents without sharing
logins. You discover the company's `invoice` agent, send it a draft request, and
get a result back. The company decides which agents and skills are published; you
never get a seat inside their deployment, only the agent endpoint.

## A2A is not MCP

These solve different problems and they compose. MCP connects one agent to its
tools and context: it answers what a single agent can call. A2A connects an
agent to other agents as peers: it answers which other agents it can hand work
to. An agent you reach over A2A may use MCP tools internally to do the job, and
you neither see nor care; the card shows skills, not the tools behind them. Use
MCP to give an agent capabilities, use A2A to let agents delegate to each other.
