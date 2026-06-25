# gh

A tiny GitHub CLI written in Koja — and a minimal example of the
stdlib HTTPS stack end to end: `HTTP.request` over
`TCPSocket.connect_tls` (BoringSSL, certificate and hostname verified
against the system trust store), with `JSON` decoding the responses.

It also shows the one-shot CLI shape for a `Process` entry: `start`
captures argv, and an overridden `run` skips the receive loop —
dispatch, print, return a `StopReason` (exit code 0 or 1).

## Layout

```
src/
  app.koja     -- entry process: argv in, exit code out
  cli.koja     -- command dispatch and text rendering (pure)
  github.koja  -- GitHub REST client: User / Repo / GitHub.fetch
  fields.koja  -- field access over decoded JSON.Value trees
test/          -- offline tests against canned API payloads
```

## Running

```sh
koja run -- user torvalds
koja run -- repos torvalds
koja run -- repo torvalds/linux
```

Or build a binary:

```sh
koja build --release
./build/release/gh user octocat
```

Unauthenticated requests are limited to 60/hour by GitHub. Set
`GITHUB_TOKEN` to a personal access token to authenticate:

```sh
GITHUB_TOKEN=ghp_... koja run -- repos my-org
```

## Tests

```sh
koja test
```

The tests are offline: they decode canned GitHub payloads and assert
on `Fields` access and `Cli` rendering. Nothing hits the network.
