# INT-009 regression — no skill ever reached a model

**Found 2026-09-24** while scoping CAP-008. INT-009 (PASS) builds "Resolve only relevant approved
skills for a task and capability snapshot". The resolver (`python/intelligence/skills/resolver.py`)
did that as a library, but nothing called it:

- a turn's context never contained a skill, whatever the registry held;
- nothing durable recorded which skill versions a run was given, so CAP-008 had no link from
  evidence to skills.

The acceptance statements were true only vacuously. "Unapproved skill version cannot enter
production context" held because no version entered at all.

**Contract (additive, DOMAIN §11.2, same commit):**

- `skill` source kind.
- BuildContext carries `skill_candidates[]` (`{skill_id, version_id, name, semver, status,
  manifest}`), `task_text`, `exposed_tools[]`, `granted_effect_classes[]` and `max_skills`.

**Plane (`python/intelligence/context/turn.py`):**

- Candidates go through INT-009's resolver: only ACTIVE versions, never widening authority
  (tool and capability needs must be within the caller's snapshot), relevance to the task,
  bounded, deterministic.
- Each resolved version becomes an optional operator segment (source `skill`, ref = version id)
  at `verified_knowledge`, rendered after the teammate's instructions and before the
  conversation, with its recovery guidance.
- Every exclusion is recorded in `degradation` with the rule that excluded it (`skill.not_active`,
  `skill.tool_not_available`, `skill.capability_not_granted`, `skill.irrelevant`,
  `resolver.budget`, `budget`).
- The Rust registry stores any manifest JSON. A version whose manifest the resolver cannot read
  is excluded and recorded, never fatal to the turn. A candidate that is not a version object
  fails closed.

**Runtime (`intelligence::model_source`):**

- Offers the registry's ACTIVE versions (`SkillStore::active_versions`), together with the run's
  exposed tools and granted effect classes.
- `max_skills` comes from `config/intelligence.yaml`.
- The recorded projection (APP-004's `record_projection`) now names the skill versions each call
  was given. That is the durable run→skill link CAP-008 needs.

**Proof:**

- `python/tests/intelligence/test_turn_context.py`, 9 new tests:
  - placement, role and trust of a resolved skill;
  - each exclusion rule recorded;
  - bounded and deterministic ordering;
  - budget drop;
  - an unreadable manifest excluded, not fatal;
  - a non-version candidate refused.
- `crates/server/tests/walking_skeleton_gateway.rs` seeds two ACTIVE skills through the registry,
  walked up the §11.5 ladder. Through the real Python process, the timeline shows:
  - the relevant skill's version in the context at `verified_knowledge`;
  - the skill needing `fs.write` excluded with `skill.tool_not_available`.
- Existing suites unchanged: skills, context bridge, gateway, stub skeleton, server lib, Python
  (613) and repository (294).

**Mutation checks** (`mutation-checks.log`). Each mutant is built first (`COMPILES`), then fails
the named suite:

1. the plane widens authority;
2. exclusions are hidden;
3. skill trust is raised;
4. the registry is not offered;
5. the runtime widens the exposed tools.
