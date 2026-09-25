# Agent Guidelines for Octopus

## Working Environment

### Do Not Start Long-Running Processes

**Rule:** Never start long-running services or jobs — e.g. frontend dev servers, backend servers, model training runs — in the background or foreground. Instead, tell the user the exact command to run; the user will run it explicitly in another terminal.

Short-lived commands (builds, tests, lints, one-off scripts) may be run directly.

## Rust Best Practices

### Model States with Enums and Match on Them

**Rule:** Represent mutually exclusive states, statuses, and conditions as `enum` variants with associated data. Branch on them with `match` (or `if let`). Never use scattered booleans, `String` constants, or raw integer codes.

#### Why

- **Type safety:** invalid states become unrepresentable.
- **Exhaustiveness:** `match` forces handling every variant.
- **Readability:** state and behavior are paired in one place.

#### Bad

```rust
// Boolean flags allow impossible combinations
struct Order {
    is_active: bool,
    is_deleted: bool,
}

// String constants are typo-prone
struct Task {
    status: String, // "pending", "running", "done"
}
```

#### Good

```rust
enum OrderStatus {
    Active { placed_at: DateTime<Utc> },
    Deleted { reason: String, deleted_at: DateTime<Utc> },
}

struct Order {
    id: Uuid,
    status: OrderStatus,
}

match order.status {
    OrderStatus::Active { placed_at } => { /* ... */ }
    OrderStatus::Deleted { reason, deleted_at } => { /* ... */ }
}
```

For one-off predicates, use `matches!`:

```rust
if matches!(ui_mode, UiMode::Acp | UiMode::Wire) { /* ... */ }
```

#### Example: Replace dynamic maps with sum types

Bad: building `HashMap<String, Value>` payloads by hand.

```rust
pub fn pre_tool_use(...) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("hook_event_name".to_string(), Value::String("PreToolUse".to_string()));
    // ... repeated, typo-prone, unchecked
    m
}
```

Good: a typed enum.

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "hook_event_name", rename_all = "PascalCase")]
pub enum HookEvent {
    PreToolUse { session_id: String, /* ... */ },
    PostToolUseFailure { session_id: String, error: String, /* ... */ },
    // ...
}
```

When the enum must also be used as a `HashMap` key, implement `PartialEq`, `Eq`, and `Hash` on the discriminant only.

#### Migration

1. Define the `enum` covering all valid states.
2. Move associated data into variants.
3. Replace old fields with a single `status: YourEnum` field.
4. Let the compiler guide fixes at all `match`/`if` sites.
5. Delete dead helpers (`is_active()`, `set_pending()`, etc.).

#### Exception

Raw primitives are acceptable only at serialization boundaries or external API interop — convert to an enum immediately at the boundary.

---

### Deserialize JSON into Typed Structs, Not `serde_json::Value`

**Rule:** When parsing JSON, derive `Deserialize` on a struct and use `response.json::<T>()` or `serde_json::from_str`. Do not deserialize into `serde_json::Value` and index fields with hardcoded strings.

Use `#[serde(default)]` and `#[serde(default = "...")]` for optional fields. Post-process after deserialization for computed values.

Reserve `serde_json::Value` for genuinely dynamic data.

#### Why

- **Type safety:** typos become compile errors.
- **Exhaustiveness:** new required fields force updates everywhere.
- **Readability:** `body.user_code` is self-documenting.

#### Bad

```rust
let body: serde_json::Value = response.json().await?;

Ok(DeviceAuthorization {
    user_code: body["user_code"].as_str().unwrap_or("").to_string(),
    device_code: body["device_code"].as_str().unwrap_or("").to_string(),
    // ...
})
```

#### Good

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorization {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: Option<u64>,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 { 5 }

let body: DeviceAuthorization = response.json().await?;
Ok(body)
```

For computed fields, deserialize first, then transform:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: f64,
    #[serde(default)]
    pub expires_in: f64,
}

let mut token: OAuthToken = serde_json::from_value(body.clone())?;
if token.expires_in > 0.0 && token.expires_at == 0.0 {
    token.expires_at = now_secs() + token.expires_in;
}
```

#### Exception

`serde_json::Value` is acceptable only when:
1. The schema is truly unknown.
2. You must inspect a small subset of fields before choosing a typed struct.

Even then, convert to a typed struct as soon as the shape is known.

---

### Use Strong Enums for Channel and IPC Messages

**Rule:** When messages flow through channels, use a single `enum` listing every message type. Do not use `serde_json::Value`, `String`, or raw bytes as the carrier type.

Use `#[serde(untagged)]` if JSON backward compatibility is required.

#### Why

- **Exhaustiveness:** `match` forces every consumer to handle every message type.
- **Discoverability:** the enum is the protocol spec.
- **Refactoring safety:** renaming a variant updates all producers/consumers.

#### Bad

```rust
pub struct Wire {
    raw_tx: broadcast::Sender<serde_json::Value>,
}

pub fn wire_send<T: Serialize>(event: T) {
    soul_side.send(serde_json::to_value(&event).unwrap());
}

// Consumer: trial-and-error deserialization
if let Ok(req) = serde_json::from_value::<ApprovalRequestEvent>(value.clone()) {
    self.pending_approval = Some(req);
} else if let Ok(text) = serde_json::from_value::<TextPart>(value) {
    self.append_text(text.text);
}
```

#### Good

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireEvent {
    TextPart(TextPart),
    TurnBegin(TurnBegin),
    ApprovalRequest(ApprovalRequestEvent),
    // ...
}

pub struct Wire {
    raw_tx: broadcast::Sender<WireEvent>,
}

match event {
    WireEvent::ApprovalRequest(req) => self.pending_approval = Some(req),
    WireEvent::TextPart(text) => self.append_text(text.text),
    // Compiler forces every variant
}
```

#### Exception

Raw bytes or `Value` are acceptable only when:
1. The channel crosses a true process/network boundary where the peer is not Rust.
2. The protocol is intentionally schemaless.

Even then, deserialize to a strong enum at the earliest possible point.

---

### Keep `mod.rs` Thin — Index Modules Only

**Rule:** `mod.rs` contains only `pub mod` declarations and `pub use` re-exports. Logic lives in sibling files.

#### Why

The index says *what* is exposed; sibling files say *how* it works.

#### Bad

```rust
pub mod toolset;
pub struct KimiSoul { /* ... */ }
impl KimiSoul { /* 800 lines */ }
```

#### Good

```rust
pub mod agent;
pub mod toolset;
mod kimisoul;
pub use kimisoul::KimiSoul;
```

#### Migration

1. Move the dominant struct/impl to a sibling file.
2. In `mod.rs`: `mod sibling_file; pub use sibling_file::MainItem;`
3. Let the compiler guide visibility fixes.

Callers stay unchanged because `mod.rs` re-exports.

#### Exception

Thin `mod.rs` indexes that re-export a sibling file may share the file's base name intentionally.

---

### Avoid Ambiguous Duplicate Module File Names

**Rule:** Do not create two module files with the same base name in the same module hierarchy if they serve different purposes. Split concerns into descriptively named files such as `types.rs`/`data.rs` for definitions and `run_*.rs`/`engine.rs` for behavior.

#### Why

Duplicate base names make imports, `grep`, stack traces, and doc links confusing.

#### Bad

```text
planner/policy/train/episode.rs         # EpisodeStep / Episode data
planner/policy/train/trainer/episode.rs # Trainer::run_episode logic
```

#### Good

```text
planner/policy/train/episode.rs             # EpisodeStep / Episode data
planner/policy/train/trainer/run_episode.rs # episode generation logic
```

#### Migration

1. Identify which file is primarily data and which is behavior.
2. Rename the behavior file to a verb-led or role-led name.
3. Update the parent `mod.rs`.
4. Run `cargo test`.

---

### Use Type-State Pipelines to Enforce Multi-Step Workflows

**Rule:**
1. When a value must pass through a fixed sequence of transformations, represent each legal intermediate state as a distinct type.
2. Only expose the operation that transitions from the current state to the next.
3. Carry shared side-effect context (RNG, transaction, accumulators) in a pipeline struct passed as mutable references.
4. Return `Result` from terminal steps so `?` propagates errors.

Never implement a multi-stage workflow as a bag of optional fields or runtime state checks.

#### Why

- **Compile-time ordering:** invalid stage sequences are type errors.
- **Locality of effects:** RNG, DB, and accumulators mutate inside one readable chain.
- **Discoverability:** the stage types document the allowed transitions.

#### Bad

```rust
struct SampleStage {
    raw: Option<EcoPlanSample<Unsimulated>>,
    simulated: Option<EcoPlanSample<Simulated>>,
    features: Option<Vec<[f64; TASK_FEATURE_DIM]>>,
}

fn process(s: &mut SampleStage) {
    if s.simulated.is_none() {
        s.simulated = Some(s.raw.take().unwrap().simulate(600.0));
    }
    let sim = s.simulated.as_ref().unwrap();
    let features = extract_sequence_features(&sim.initial_eco, &sim.plan);
    insert_sample(&tx, &features, sim).unwrap();
}
```

#### Good

```rust
// docref: demo
struct BuildPipeline<'a> {
    request: &'a BuildRequest,
    library: &'a BlueprintLibrary,
    clock: &'a mut SchedulerClock,
}

impl<'a> BuildPipeline<'a> {
    fn select_action(self) -> ActionSelected<'a> { ... }
}

struct ActionSelected<'a> { ... }
impl<'a> ActionSelected<'a> {
    fn simulate(self) -> SimulatedAction<'a> { ... }
}

struct SimulatedAction<'a> { ... }
impl<'a> SimulatedAction<'a> {
    fn commit(self) -> CommittedAction<'a> { ... }
}

struct CommittedAction<'a> { ... }
impl<'a> CommittedAction<'a> {
    fn update_world(self) -> Result<()> { ... }
}
```

Usage:

```rust
// docref: demo
BuildPipeline { request, library, clock }
    .select_action()
    .simulate()
    .commit()
    .update_world()
    .with_context(|| "Failed to apply build step")?;
```

#### Migration

1. List the fixed sequence of operations.
2. Create one stage type per intermediate state.
3. Create a context struct that owns the shared resources and mutable accumulators.
4. Implement only the single outgoing transition on each stage type.
5. Replace imperative steps with the chain and delete runtime checks.

#### Exception

- Simple one-step transformations do not need a pipeline; use a regular function.
- Dynamic, branching, or cyclic workflows are better modeled as enums or explicit state machines.

---

## Documentation Code Snippets

### Always Annotate Code Snippets with Source Location

**Rule:** Every code snippet in documentation that shows project source code must begin with a source-location comment containing:
1. **File path** — relative to project root.
2. **Approximate line number** — prefixed with `~line`.
3. **Function or item name**.

#### Why

- **Traceability:** readers can grep and jump to the definition.
- **Drift detection:** outdated line numbers signal that the doc needs updating.
- **Reviewability:** reviewers can verify snippets against source.

#### Good

```rust
// octopus-cli/src/hooks/engine.rs ~line 206 — HookEngine::trigger
pub async fn trigger(&self, event: HookEvent, matcher_value: &str) -> Vec<HookResult> {
    // ...
}
```

#### Abbreviated Snippets

If a snippet is intentionally truncated, add `(abbreviated)`:

```rust
// octopus-cli/src/hooks/engine.rs ~line 59 — HookEngine (abbreviated)
pub struct HookEngine {
    hooks: Vec<HookDef>,
    // ... callbacks omitted
}
```

#### Pseudo-Code / Demo Snippets

Mark conceptual or illustrative snippets:

```rust
// docref: demo
fn how_to_clone<T: Clone>(item: T) -> (T, T) {
    (item.clone(), item.clone())
}
```

#### Exception

- External examples (user-written scripts) do not need source-location comments.
- Shell commands and JSON examples do not need source-location comments unless machine-generated.
