# Map a provider to TokenCraft primitives

A mapping file describes how one provider model translates TokenCraft fields to its request body and how its result translates back. Keep the file with the connector that uses it. One provider can have several model mappings, and each mapping identifies the provider, upstream model, primitive, schema ID, and revision.

The shared [primitive schema registry](../crates/core/provider-mapping/schemas/v1.json) covers text, image, video, audio, mesh, and world. A running gateway serves it at `GET /v1/schemas/primitives`. These definitions describe connector payloads after quote and workload selection. The queue gateway currently executes image, video, audio, and mesh. The text input uses the body sent to the compatible chat binding, including its model ID and flat sampling options. World maps can be developed and tested with the registry as a preview for future gateway support.

## Try the example

From the native package directory, run the bundled CLI. From a source checkout, run `cargo build --locked -p tokencraft` and use `./target/debug/tokencraft` in the commands below. The examples run locally and need only the mapping and input files.

```sh
./bin/tokencraft mapping schema --primitive image
./bin/tokencraft mapping list --dir examples/mapping
./bin/tokencraft mapping preview --file examples/mapping/example-image.map.json \
  --direction request --input examples/mapping/request.json
./bin/tokencraft mapping preview --file examples/mapping/example-image.map.json \
  --direction response --input examples/mapping/response.json
```

The [example mapping](../examples/mapping/example-image.map.json) turns the canonical `prompt`, `count`, and `aspect_ratio` into the example provider's description, output count, and layout. Its response rules reshape a list of provider files into image references. The bundled Rust connector uses the request mapping when it generates its PNGs.

The custom chat example renames message fields on each item, carries sampling settings through when present, and maps provider token counts back into the shared result. Preview it with the [chat request](../examples/mapping/chat-request.json) and [provider response](../examples/mapping/chat-response.json). [Chat mapping](../examples/mapping/custom-chat.map.json)

## Draft a mapping with AI

Configure the complete chat completions endpoint of a compatible mapper model. For a local service the URL can use a literal loopback address. If the service requires a key, supply it in `TOKENCRAFT_MAPPER_API_KEY`.

```sh
export TOKENCRAFT_MAPPER_ENDPOINT=http://127.0.0.1:8080/v1/chat/completions
export TOKENCRAFT_MAPPER_MODEL=your-mapper-model

tokencraft mapping draft --primitive image --provider my-provider \
  --model vendor/image-v2 --revision v1 \
  --context provider-context.md > image-v2.draft.map.json
```

Use `--url https://docs.example.com/models/image-v2` in place of `--context` to fetch public HTTPS documentation. For large pages or pages that need sign-in or JavaScript, save a UTF-8 excerpt with the model's request and response examples. The input can contain up to 64 KiB. The command sends the selected documentation and primitive definitions to the configured mapper and records a source reference and SHA-256 digest on the draft.

The AI returns JSON suggestions and notes. Edit the file to resolve field names, units, defaults, and unsupported options. Try both directions on representative samples. Approve the mapping after reviewing the translated payloads.

```sh
tokencraft mapping validate --file image-v2.draft.map.json
tokencraft mapping approve --file image-v2.draft.map.json \
  --request canonical-request.json --response upstream-response.json \
  > image-v2.map.json
```

Approval validates both samples locally and writes a new approved mapping to stdout. Keep the draft, approved file, and test samples in your connector's source control. Increase the mapping revision when behavior changes. As with any binding change, retain the prior connector revision while existing jobs drain.

## Edit the rules

Each rule has a `to` JSON Pointer and a `value` operation. `from` is a JSON Pointer on the source document. An empty `from` reads the entire source value or array item. Target paths write objects. Use `each` to build an array of objects.

| Operation | Purpose |
| --- | --- |
| `copy` | Rename or move a value |
| `literal` | Supply a fixed JSON value |
| `lookup` | Translate a canonical string through an explicit table |
| `multiply` | Scale a signed integer by an integer factor, for example seconds to milliseconds |
| `each` | Apply relative field rules to each element of a list |

Set `optional: true` on a rule when an absent source is valid. Use `ignore_request` for explicit fields that have served their purpose, such as the connector input's `/kind` discriminator. Preview reports a supplied request field that has no rule and is absent from this list. If a model lacks a requested capability, limit the corresponding catalog offer and allow incompatibilities to surface during review. Field paths use [RFC 6901 JSON Pointer](https://www.rfc-editor.org/rfc/rfc6901).

## Use the mapping in a connector

Use the mapping's `provider` as the catalog provider ID and its `model` as `upstream_model`. The project provider preview accepts an optional `provider_id` on the attach operation and carries that identity into the catalog. Models added to the same binding inherit its provider ID.

Rust connectors can use the `provider_mapping` crate and call `Mapping::apply` for the selected provider, model, registry, and direction. Other connectors can call the same transformation through the CLI. Use `--input -` to read a JSON payload from stdin.

```sh
tokencraft mapping apply --file image-v2.map.json --provider my-provider \
  --model vendor/image-v2 --direction request --input canonical-request.json
```

Apply enforces review status and offer identity. The connector owns the upstream endpoint, credentials, submission, polling, and artifact transfer. Normalized media results can hold upstream artifact locations during connector processing. Return the bytes through the existing [connector protocol](provider-connectors.md) so the gateway can validate and publish its own artifact URLs. For text, use the mapped result in a compatible chat response. Connectors with streaming can apply equivalent event transformations under their own transport contract.

## Extend the open schema

Copy the [registry JSON](../crates/core/provider-mapping/schemas/v1.json) and assign it a new `$id` such as `urn:tokencraft:primitives:local-1`. Use `--schema path/to/registry.json` with the CLI and reference that ID in your mapping. Add fields under the appropriate `properties` or add a pair of `<primitive>_input` and `<primitive>_result` object definitions. Use paired fixtures to test your additions before proposing them upstream.

The registry uses a bounded subset of JSON Schema 2020-12: explicit object properties and required fields, typed arrays, scalar types and type unions, enums, and length, item, or numeric bounds. Definitions are inline. Unsupported keywords produce an error so contributors can tell which constraints are enforced. [JSON Schema 2020-12](https://json-schema.org/draft/2020-12)

To expose a new field through the gateway itself, carry it through the API input, validated workload, catalog capability, and connector wire type. Add a round-trip test for that path and update the published registry with the change. Schema-only extensions can be used by independent connectors and mapping tools.
