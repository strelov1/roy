use roy_management::cwd::{resolve_cwd, CwdInput, CwdScope};

fn ws() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("roy-cwd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn personal_session_no_project() {
    let ws = ws();
    let p = resolve_cwd(
        &ws,
        CwdInput {
            scope: CwdScope::Personal,
            user_id: "U1".into(),
            team_id: None,
            project_id: None,
            session_id: "S1".into(),
        },
    )
    .unwrap();
    assert_eq!(p, ws.join("users").join("U1").join("sessions").join("S1"));
}

#[test]
fn team_session_with_project() {
    let ws = ws();
    let p = resolve_cwd(
        &ws,
        CwdInput {
            scope: CwdScope::Team,
            user_id: "U1".into(),
            team_id: Some("T1".into()),
            project_id: Some("P1".into()),
            session_id: "S1".into(),
        },
    )
    .unwrap();
    assert_eq!(
        p,
        ws.join("teams")
            .join("T1")
            .join("projects")
            .join("P1")
            .join("sessions")
            .join("S1")
    );
}

#[test]
fn path_traversal_rejected() {
    let ws = ws();
    let err = resolve_cwd(
        &ws,
        CwdInput {
            scope: CwdScope::Personal,
            user_id: "../../etc".into(),
            team_id: None,
            project_id: None,
            session_id: "S1".into(),
        },
    );
    assert!(err.is_err());
}

#[test]
fn non_uuid_id_rejected() {
    let ws = ws();
    let err = resolve_cwd(
        &ws,
        CwdInput {
            scope: CwdScope::Personal,
            user_id: "alice/bob".into(),
            team_id: None,
            project_id: None,
            session_id: "S1".into(),
        },
    );
    assert!(err.is_err());
}
