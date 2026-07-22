//! Regression tests for issue #5929 — Toxrill, the Corrosive: "Creatures you
//! don't control get -1/-1 for each slime counter on them."
//!
//! Reported symptom: slime counters accumulated but never applied -1/-1.
//!
//! Root cause: the legacy for-each clause parser's wildcard "counter on"
//! fallback (`oracle_quantity.rs::parse_for_each_clause`) mapped the plural
//! anaphor "slime counter on them" to `CountersOn { scope: Source }` — the
//! static's own source (Toxrill) bears no slime counters, so the dynamic
//! -1/-1 always resolved to -0/-0. "On them" names each AFFECTED creature, so
//! the scope must be `Recipient`, bound per-object during layer evaluation
//! (CR 613.4c). Fixed by a dedicated nom combinator
//! (`parse_for_each_counters_on_them`, `oracle_nom/quantity.rs`) tried before
//! the wildcard, plus registering `CountersOn { scope: Recipient }` in
//! `quantity_expr_uses_recipient` so layers defer resolution into the
//! per-recipient loop.

use super::rules::{GameScenario, Phase, WaitingFor, P0, P1};
use engine::game::layers::evaluate_layers;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;

fn pt(runner: &super::rules::GameRunner, obj: ObjectId) -> (i32, i32) {
    runner
        .state()
        .objects
        .get(&obj)
        .map(|o| (o.power.unwrap_or(0), o.toughness.unwrap_or(0)))
        .unwrap_or((0, 0))
}

const TOXRILL: &str = "At the beginning of each end step, put a slime counter on each creature you don't control.\nCreatures you don't control get -1/-1 for each slime counter on them.\nWhenever a creature you don't control with a slime counter on it dies, create a 1/1 black Slug creature token.\n{U}{B}, Sacrifice a Slug: Draw a card.";

fn slime() -> CounterType {
    CounterType::Generic("slime".to_string())
}

/// Direct layer assertion: seeded slime counters must shrink each opposing
/// creature by its OWN counter count, leave the controller's creatures alone,
/// and never read the source's counters.
#[test]
fn toxrill_slime_counters_apply_minus_one_minus_one_per_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _toxrill = scenario
        .add_creature_from_oracle(P0, "Toxrill, the Corrosive", 7, 7, TOXRILL)
        .id();
    let one_counter = scenario.add_creature(P1, "One Slime Victim", 3, 3).id();
    let two_counters = scenario.add_creature(P1, "Two Slime Victim", 4, 4).id();
    let clean = scenario.add_creature(P1, "Clean Opponent", 2, 2).id();
    let friendly = scenario.add_creature(P0, "Friendly", 2, 2).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&one_counter)
            .unwrap()
            .counters
            .insert(slime(), 1);
        state
            .objects
            .get_mut(&two_counters)
            .unwrap()
            .counters
            .insert(slime(), 2);
        evaluate_layers(state);
    }

    assert_eq!(
        pt(&runner, one_counter),
        (2, 2),
        "one slime counter must apply exactly -1/-1"
    );
    assert_eq!(
        pt(&runner, two_counters),
        (2, 2),
        "two slime counters must apply -2/-2 (per-recipient read, not shared)"
    );
    assert_eq!(
        pt(&runner, clean),
        (2, 2),
        "an opposing creature without slime counters must be unaffected"
    );
    assert_eq!(
        pt(&runner, friendly),
        (2, 2),
        "the controller's own creatures must be unaffected"
    );
}

/// End-to-end: Toxrill's own end-step trigger places the slime counter, and
/// the anthem must then apply -1/-1 through the real trigger → counter →
/// layer pipeline.
#[test]
fn toxrill_end_step_counter_then_debuff_end_to_end() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _toxrill = scenario
        .add_creature_from_oracle(P0, "Toxrill, the Corrosive", 7, 7, TOXRILL)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 3, 3).id();

    let mut runner = scenario.build();

    // Drive to P0's end step; the "at the beginning of each end step" trigger
    // puts a slime counter on the opposing creature.
    for _ in 0..60 {
        let at_end = runner.state().phase == Phase::End;
        let stack_len = runner.state().stack.len();
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if at_end && stack_len == 0 {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            other => panic!("unexpected WaitingFor while advancing to end step: {other:?}"),
        }
    }

    assert_eq!(
        runner
            .state()
            .objects
            .get(&victim)
            .unwrap()
            .counters
            .get(&slime())
            .copied()
            .unwrap_or(0),
        1,
        "the end-step trigger must have placed exactly one slime counter"
    );
    assert_eq!(
        pt(&runner, victim),
        (2, 2),
        "the slime counter must apply -1/-1 through the real pipeline"
    );
}

/// The mixed source/recipient runtime regression the maintainer requested on
/// the sibling attempt (PR #6335): ONE static whose dynamic terms read
/// counters both "on them" (per recipient) and "on ~" (the source). Each
/// quantity must keep its own provenance — the recipient term reads the
/// affected creature's slime counters, the source term reads the source's
/// storage counters, and both apply to the recipient.
#[test]
fn mixed_recipient_and_source_counter_reads_keep_per_quantity_provenance() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Mixed Reader",
            2,
            2,
            "Creatures you don't control get -1/-1 for each slime counter on them and -1/-1 for each storage counter on ~.",
        )
        .id();
    let victim = scenario.add_creature(P1, "Victim", 5, 5).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&victim)
            .unwrap()
            .counters
            .insert(slime(), 1);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .counters
            .insert(CounterType::Generic("storage".to_string()), 2);
        evaluate_layers(state);
    }

    assert_eq!(
        pt(&runner, victim),
        (2, 2),
        "victim must take -1/-1 (its own slime counter) plus -2/-2 (the source's \
         two storage counters): per-quantity provenance, no scope leaking"
    );
    assert_eq!(
        pt(&runner, source),
        (2, 2),
        "the source itself is not an affected object and must be untouched"
    );
}
