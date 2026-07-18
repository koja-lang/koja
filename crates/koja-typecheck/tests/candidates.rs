use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::{Candidate, CandidateKind};

mod common;

use common::{PACKAGE, typecheck_script as typecheck};

fn sorted(candidates: &[Candidate<'_>]) -> bool {
    candidates.windows(2).all(|pair| {
        let left = &pair[0];
        let right = &pair[1];
        (left.kind, left.label) <= (right.kind, right.label)
    })
}

#[test]
fn dot_candidates_use_stable_kind_then_label_order() {
    let source = "
        struct Sample
          zebra: Int
          alpha: Int

          fn middle(self) -> Int
            self.alpha
          end
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    let identifier = Identifier::new(PACKAGE, vec!["Sample".to_string()]);
    let (sample_id, _) = checked
        .registry
        .lookup(&identifier)
        .expect("Sample should be registered");
    let candidates = checked.registry.dot_candidates(sample_id, false);

    assert!(
        sorted(&candidates),
        "candidates are not sorted: {candidates:?}"
    );
    let fields: Vec<&str> = candidates
        .iter()
        .filter(|candidate| candidate.kind == CandidateKind::Field)
        .map(|candidate| candidate.label)
        .collect();
    assert_eq!(fields, vec!["alpha", "zebra"]);
}

#[test]
fn symbol_candidates_use_stable_kind_then_label_order() {
    let source = "
        fn zebra -> Int
          1
        end

        struct Middle
        end

        fn alpha -> Int
          1
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    let candidates = checked.registry.symbol_candidates(PACKAGE, PACKAGE);

    assert!(
        sorted(&candidates),
        "candidates are not sorted: {candidates:?}"
    );
    let functions: Vec<&str> = candidates
        .iter()
        .filter(|candidate| candidate.kind == CandidateKind::Function)
        .map(|candidate| candidate.label)
        .collect();
    assert_eq!(functions, vec!["alpha", "zebra"]);
}
