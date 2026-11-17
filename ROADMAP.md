# Merry Roadmap

Detailed scope, acceptance, evidence, and status live in the linked GitHub
issues. This file keeps only the major delivery sequence and its dependencies.

## Delivery Status

T0 through T8 are complete. T10 ownership-based architecture governance is also
complete. T11 release closure remains open. The Harbor benchmark and the
`merry-eval` TaskSpec prototype were removed: external evaluation is not a
product requirement.

## Initiative

- [ ] [Parent: Establish an evaluable, reusable, production-grade Rust coding agent boundary](https://github.com/locez/merry/issues/9)
- [x] [T0: Establish delivery baseline, roadmap, and architecture guard](https://github.com/locez/merry/issues/10)
- [x] [T1: Establish evaluation protocol and TaskSpec](https://github.com/locez/merry/issues/8) (closed as a prototype; `merry-eval` was later removed)
- [x] [T2: Deterministic Offline Harness (closed; covered by Rust unit/integration tests)](https://github.com/locez/merry/issues/11)
- [x] [T3: Establish the unique CodingAgentProfile (implemented; shared facade/CLI profile, stable-prefix and admission evidence)](https://github.com/locez/merry/issues/12)
- [x] [T4: Internal Coding Eval Suite (closed; no standalone runner)](https://github.com/locez/merry/issues/13)
- [x] [T5: Unify CLI, Debug, and Rust Runtime Builder](https://github.com/locez/merry/issues/14) (shared builder, validated permission policy, parent-child inheritance tests, full Rust verification; read-only capability-closure follow-up deferred by approval)
- [x] [T6: Establish production reliability, security, and observability harness](https://github.com/locez/merry/issues/15) (closed; deterministic PR gate, security/lifecycle regression coverage, and structured failure evidence)
- [x] [T7: Release a stable Rust SDK](https://github.com/locez/merry/issues/16) (closed; typed facade, agent lifecycle, public event streams, and tool registration)
- [x] [T8: Align Python and Rust SDK capabilities](https://github.com/locez/merry/issues/17) (closed; typed async SDK, Rust-owned lifecycle, capability-parity tests, and packaging checks)
- T9 external coding-agent evaluation was dropped. [Issue #18](https://github.com/locez/merry/issues/18) is closed. The Harbor adapter, the smoke workflow, and the `benchmark/` package were removed because external benchmarks are not required.
- [x] [T10: Split large Runtime and Binding modules by responsibility](https://github.com/locez/merry/issues/19) (ownership-based runtime, tools, CLI, and test decomposition; full Rust/Python verification)
- [ ] [T11: Establish release gates and close the Definition of Done](https://github.com/locez/merry/issues/20)

## Dependency Order

```text
T0 -> T3
T3 -> T5
T5 -> T6
T5 + T6 -> T7 -> T8
T5 + T7 + T8 -> T10
T6 + T10 -> T11
```

T1's TaskSpec prototype in `merry-eval` was removed with the Harbor benchmark.
T2 and T4 stay closed: deterministic Rust tests cover runtime contracts. T9
no longer gates release closure.

The architecture ownership and dependency contract is maintained in
`AGENTS.md`; issue descriptions remain the source of truth for implementation
details and progress.
