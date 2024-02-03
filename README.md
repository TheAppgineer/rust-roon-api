# rust-roon-api
A [Rust](https://www.rust-lang.org) port of the RoonLabs [node-roon-api](https://github.com/RoonLabs/node-roon-api) and its optional services

## Current stage
This API is currently in an Alpha stage of development. Main reason for starting this project is learning about the Rust programming language by getting my hands dirty.

## Features
The official RoonLabs Node Roon API provides optional services via separate git repositories to be specified in the `package.json` file as dependencies. This Rust port uses the Cargo features mechanism to only include the needed services in the compiled binary.

|Node.js|Feature|State|
|---|---|---|
|node-roon-api|pairing|Ported|
|node-roon-api-status|status|Ported|
|node-roon-api-settings|settings|Ported|
|node-roon-api-transport|transport|Ported|
|node-roon-api-browse|browse|Ported|
|node-roon-api-image|image|Ported|
|node-roon-api-volume-control||Not Ported|
|node-roon-api-source-control||Not Ported|

### Notes
Each feature is implemented in its own file. The file contains some test code that can act as a basic example of that specific feature.

When the `pairing` feature is not used then the `core_found` / `core_lost` functionality of the node API is replicated.

Dependencies of features are handled automatically, meaning that `pairing` doesn't have to be specified for `transport` and `browse`.

## Not on crates.io
The Rust Roon API cannot be found on cates.io, instead the git repository has to be specified in the `Cargo.toml` file:

```
[dependencies]
rust-roon-api = { git = "https://github.com/TheAppgineer/rust-roon-api.git", tag = "0.2.0", features = ["settings", "status", "transport"] }
```

To put the API into use you also depend on Tokio and Serde

```
[dependencies]
tokio = { version = "1.24", features = ["macros", "rt"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

## A Real-Life Application
An API implementation can only prove its usability by being used in a real-life application. For this reason I created [Roon TUI](https://github.com/TheAppgineer/roon-tui). The first extension that is written from the ground up in Rust.
