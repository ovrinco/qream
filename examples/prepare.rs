//! A local campaign preparation and persistence example with synthetic rows.
use qream::engine::{
    folder::Campaign,
    rows::{Group, MemoryRows, Row},
    setup::{self, Scope},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("usage: cargo run --example prepare -- /empty/path")?;
    let root = std::path::PathBuf::from(root);
    let mut source = MemoryRows::default();
    source.rows.insert(
        "EXAMPLE-001".into(),
        Row {
            id: "EXAMPLE-001".into(),
            purpose:
                "Prepare a repeatable synthetic campaign with its original requirement preserved."
                    .into(),
            intent: "Keep the assignment understandable to its reviewer.".into(),
            dod: "The campaign reopens and retains the selected requirement.".into(),
            exemplar: "A small document review.".into(),
            source: "example:requirement:1".into(),
            status: "said".into(),
            outcome_id: "EXAMPLE-OUTCOME".into(),
        },
    );
    source.groups.insert("EXAMPLE-OUTCOME".into(), Group {
        id: "EXAMPLE-OUTCOME".into(),
        outcome: "A reviewer can reopen a synthetic campaign with its selected requirement preserved.".into(),
        standing: "said".into(),
        proof: "Prepare, reopen and refuse a conflicting creation.".into(),
    });
    let scope = Scope {
        rows: vec!["EXAMPLE-001".into()],
        groups: vec![],
    };
    setup::new(&root, "example", &scope, &source)?;
    let reopened = Campaign::open(&root)?;
    println!("Prepared and reopened: {}", reopened.root.display());
    assert!(root.join("OUTCOME.md").is_file());
    assert!(setup::new(&root, "example", &scope, &source).is_err());
    println!("A second creation was refused without replacing the campaign.");
    Ok(())
}
