# Deep Nested Update & Delete Implementation Plan

## Objective
Implement deep nested `update` and `delete` operations within the `caqui` engine, strictly enforcing Scoped Security and Polymorphic Disambiguation (via the `__kind` discriminator). This allows clients to deeply mutate existing relational graphs in a single, high-performance transaction.

## Phase 1: AST and Payload Parsing Enhancements
**Goal:** Equip the `api-layer` mutation translator to correctly parse nested `update` and `delete` blocks, including polymorphic arrays.

1. **`crates/api-layer/src/mutation_translator.rs`:**
   - **Update Singular Relations (1:1/N:1):** Ensure `update` and `delete` payloads are supported as objects (not just arrays) for singular relational fields.
   - **Update Array Relations (1:N):** Ensure `update` and `delete` payloads are parsed correctly when presented as an array of operations.
   - **Polymorphic Validation:** In the nested mutation loop, when processing an `update` or `delete` action on a Polymorphic field (`Union` or `Base`), enforce the presence of the `__kind` discriminator inside the `where` block.
   - **Payload Extraction:** Extract the target `__kind` and use it to validate the `data` payload against the specific concrete `ModelNode` in the AST, rather than the abstract `Base`.

## Phase 2: Compiler and Execution Step Generation
**Goal:** Transform the parsed nested payloads into logical execution steps that guarantee "Scoped Security."

1. **`crates/api-layer/src/mutation_translator.rs`:**
   - **`DeferredAction::Update`:** Map this to a new `ExecutionStep::UpdateBranch`.
     - The compiler must construct a parameterized `WhereClause` combining the user's `where` block with a strict structural constraint: `foreignKeyColumn = ?parent_id`.
   - **`DeferredAction::Delete`:** Map this to a new `ExecutionStep::DeleteBranch`.
     - Similarly, construct a parameterized `WhereClause` enforcing `foreignKeyColumn = ?parent_id`.
   - **Polymorphic Generation:** For polymorphic relations, the target table for the SQL statement is determined by the `__kind` field extracted in Phase 1. The generated `ExecutionStep` must target this concrete table, NOT the base.

## Phase 3: The Execution Engine (SQL Generation)
**Goal:** Update the transaction executor to physically process the new branching steps.

1. **`crates/api-layer/src/executor.rs`:**
   - **`ExecutionStep::UpdateBranch`:**
     - Resolve the parent's `__id` from the context (using `root_step_id` or similar reference tracking).
     - Dynamically construct an `UPDATE <table> SET <fields> WHERE <user_where> AND <fk_col> = ?` SQL statement.
     - Bind the parameterized values (including the parent ID) and execute.
   - **`ExecutionStep::DeleteBranch`:**
     - Construct a `DELETE FROM <table> WHERE <user_where> AND <fk_col> = ?` statement.
     - Bind the parameterized values and execute.
   - **Error Handling:** If an `UpdateBranch` or `DeleteBranch` affects 0 rows, determine if this should be a silent pass or a strict failure (typically, nested operations are strict. If a user asks to delete a specific ID that they own and it doesn't exist, an error is thrown).

## Phase 4: E2E Testing and Validation
**Goal:** Exhaustively test the implementation against standard and polymorphic relationships.

1. **`crates/engine-core/tests/e2e_advanced_mutations.rs`:**
   - **Nested Update (Standard):** Create a User with Posts. Issue a nested `update` to change a Post's title. Verify the database updates correctly.
   - **Nested Delete (Standard):** Issue a nested `delete` for a Post. Verify the Post is removed.
   - **Scoped Security Test:** Attempt to issue a nested `update` on a Post that belongs to a *different* User. Verify the operation fails (or affects 0 rows and triggers a rollback) due to the injected FK constraint.
   - **Polymorphic Nested Update:** Create a Collection with Articles and Videos (Polymorphic Base). Issue a nested `update` using `__kind: "Article"` within the `where` block. Verify the specific Article is updated.
   - **Polymorphic Nested Delete:** Issue a nested `delete` using `__kind: "Video"`. Verify the Video is deleted.