//! HCV-02 ownership drift check (foundation `03 platform/16` §16.5, owner
//! decision 2026-08-23): the compiler↔host contract text is owned by
//! `clean-language-compiler/contracts/`; this repo's root `host.wit` is a
//! byte-verified copy. Runs against the sibling compiler checkout; skips
//! (loudly) when it is absent, UNLESS `CLEAN_SPEC_REQUIRED` is set — CI
//! clones the owner and sets that variable, so a conformance test can
//! never self-skip there.

use std::path::PathBuf;

#[test]
fn host_wit_matches_contract_owner() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();
    let ours = repo_root.join("host.wit");
    let owner = repo_root
        .parent()
        .expect("parent of repo root")
        .join("clean-language-compiler/contracts/host.wit");

    if !owner.is_file() {
        assert!(
            std::env::var_os("CLEAN_SPEC_REQUIRED").is_none(),
            "CLEAN_SPEC_REQUIRED is set but ../clean-language-compiler is absent — \
             a conformance test must not self-skip when the owner checkout was promised"
        );
        eprintln!("SKIP: ../clean-language-compiler not present; drift leg runs locally only");
        return;
    }

    let ours_bytes = std::fs::read(&ours).expect("read root host.wit");
    let owner_bytes = std::fs::read(&owner).expect("read owner contracts/host.wit");
    assert_eq!(
        ours_bytes, owner_bytes,
        "host.wit drifted from the contract owner \
         (clean-language-compiler/contracts/host.wit). A contract change starts \
         there and is propagated here deliberately — refresh this copy in its \
         own commit, never edit it first (HCV-02 ownership, 2026-08-23)."
    );
}
