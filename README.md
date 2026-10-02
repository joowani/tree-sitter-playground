# Tree-sitter Playground

A web playground for exploring [Tree-sitter](https://tree-sitter.github.io) syntax trees. Type or paste code to see
its syntax tree update as you type, with each node's kind, field name, and position. Clicking a node selects its code,
and **Auto-detect** picks the language for you.

## Installation

With Docker:

```shell
docker run --rm -p 3000:3000 ghcr.io/joowani/tree-sitter-playground
```

With Rust 1.90 or later:

```shell
cargo install --locked tree-sitter-playground
tree-sitter-playground
```

Then open <http://localhost:3000>. Use `--host` and `--port` to listen elsewhere.

## Supported Languages

`c`, `cpp`, `csharp`, `go`, `graphql`, `hcl`, `java`, `javascript`, `json`, `jsx`, `kotlin`, `php`, `prisma`,
`protobuf`, `python`, `ruby`, `rust`, `sql`, `swift`, `terraform`, `thrift`, `toml`, `tsx`, `typescript`, `yaml`

