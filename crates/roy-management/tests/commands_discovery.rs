use roy_management::commands::list_commands_from;

#[tokio::test]
async fn scans_roy_skills_dir() {
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().join(".roy/skills/review");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: review\ndescription: Review a PR\n---\n\nBody.",
    )
    .unwrap();
    let out = list_commands_from(dir.path()).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "review");
    assert_eq!(out[0].description, "Review a PR");
    assert_eq!(out[0].source, "roy");
}

#[tokio::test]
async fn ignores_claude_skills_dir() {
    let dir = tempfile::tempdir().unwrap();
    // A SKILL.md only under ~/.claude/skills/ must not surface.
    let skills = dir.path().join(".claude/skills/legacy");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: legacy\ndescription: old\n---\n\nbody.",
    )
    .unwrap();
    let out = list_commands_from(dir.path()).await;
    assert_eq!(out.len(), 0);
}

#[tokio::test]
async fn empty_dir_returns_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let out = list_commands_from(dir.path()).await;
    assert_eq!(out.len(), 0);
}

#[tokio::test]
async fn malformed_frontmatter_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().join(".roy/skills/bad");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\ndescription: No name here\n---\n",
    )
    .unwrap();
    let out = list_commands_from(dir.path()).await;
    assert_eq!(out.len(), 0);
}
