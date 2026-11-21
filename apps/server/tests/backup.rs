use std::path::Path;
use std::process::{Command, Output};

use owlee_platform::{Database, DatabaseConfig};
use owlee_server::{boot, editor_session, testing_config};
use sqlx::Row;

fn owlee(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_owlee"))
        .args(args)
        .env_clear()
        .env("OWLEE_ENV", "development")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

async fn email(path: &Path, user_id: &str) -> String {
    let database = Database::connect(&DatabaseConfig::sqlite(path))
        .await
        .unwrap();
    sqlx::query("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(database.pool())
        .await
        .unwrap()
        .get("email")
}

#[tokio::test]
async fn backs_up_a_database_that_is_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("owlee.sqlite");
    let copy = dir.path().join("backup.sqlite");
    let state = boot(testing_config(live.clone())).await.unwrap();
    let user_id = editor_session(&state).await.unwrap().user_id.to_string();
    assert!(
        dir.path()
            .join("owlee.sqlite-wal")
            .metadata()
            .unwrap()
            .len()
            > 0,
        "the new row is still in the WAL"
    );

    let args = [
        "backup",
        "--database",
        live.to_str().unwrap(),
        copy.to_str().unwrap(),
    ];
    let first = owlee(&args, &[]);
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first.status.success(), "{first:?}");
    let expected = format!("backed up {} to {} (", live.display(), copy.display());
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with(&expected) && line.ends_with(" KiB)")),
        "{stdout}"
    );
    assert_eq!(
        email(&copy, &user_id).await,
        email(&live, &user_id).await,
        "the copy holds the row written through the running pool"
    );

    let again = owlee(&args, &[]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already exists"));
}

#[test]
fn postgres_points_to_pg_dump() {
    let output = owlee(
        &["backup", "/tmp/never-written.sqlite"],
        &[("OWLEE_DATABASE_URL", "postgres://owlee@localhost/owlee")],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pg_dump"));
    assert!(!Path::new("/tmp/never-written.sqlite").exists());
}
