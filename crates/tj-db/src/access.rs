//! Project membership is the common API/Chief authorization boundary.
use super::*;

pub(super) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_members (
        project_id TEXT NOT NULL, user_id TEXT NOT NULL,
        PRIMARY KEY(project_id, user_id)
    ); CREATE TRIGGER IF NOT EXISTS remove_project_members AFTER DELETE ON projects
    BEGIN DELETE FROM project_members WHERE project_id = OLD.id; END;
    CREATE TRIGGER IF NOT EXISTS remove_user_memberships AFTER DELETE ON users
    BEGIN DELETE FROM project_members WHERE user_id = OLD.id; END;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        let db = Database {
            conn: Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
        };
        db.migrate().unwrap();
        for id in ["admin", "alice", "bob"] {
            db.create_user(&User {
                id: id.into(),
                username: id.into(),
                password_hash: "unused".into(),
                display_name: None,
                is_admin: id == "admin",
                created_at: String::new(),
            })
            .unwrap();
        }
        for id in ["a", "b"] {
            db.save_project(&Project {
                id: id.into(),
                name: id.into(),
                repo_url: None,
                base_branch: "main".into(),
                workspace_path: None,
                git_provider: None,
                profile: "full".into(),
                workspace_kind: None,
                catalog_id: None,
                provisioning_state: None,
                provisioning_error: None,
                created_at: String::new(),
                updated_at: String::new(),
            })
            .unwrap();
            db.save_run(&PipelineRun::new(
                format!("run-{id}"),
                Some(ProjectId(id.into())),
                None,
                id.into(),
                RunStatus::Running,
                None,
                None,
                None,
                None,
            ))
            .unwrap();
        }
        db
    }

    #[test]
    fn membership_is_explicit_revocable_and_project_scoped() {
        let db = db();
        assert!(!db.can_access_project("alice", "a").unwrap());
        db.set_project_member("admin", "a", "alice", true).unwrap();
        db.set_project_member("admin", "b", "bob", true).unwrap();
        assert!(db.can_access_run("alice", "run-a").unwrap());
        assert!(!db.can_access_run("alice", "run-b").unwrap());
        assert!(!db.can_access_run("bob", "run-a").unwrap());
        assert!(db.set_project_member("alice", "b", "alice", true).is_err());
        assert!(!db.can_access_project("alice", "b").unwrap());
        db.set_project_member("admin", "a", "alice", false).unwrap();
        assert!(!db.can_access_run("alice", "run-a").unwrap());
        assert!(db.can_access_run("admin", "run-a").unwrap());
        assert!(!db.can_access_run("missing-user", "run-a").unwrap());
    }

    #[test]
    fn deleted_project_membership_does_not_survive_recreation() {
        let db = db();
        db.set_project_member("admin", "a", "alice", true).unwrap();
        let project = db.load_project("a").unwrap().unwrap();
        db.delete_project("a").unwrap();
        db.save_project(&project).unwrap();
        assert!(!db.can_access_run("alice", "run-a").unwrap());
    }

    #[test]
    fn artifacts_and_parentless_runs_cannot_escape_project_boundary() {
        let db = db();
        db.set_project_member("admin", "a", "alice", true).unwrap();
        db.save_run(&PipelineRun::new(
            "legacy".into(),
            None,
            None,
            "legacy".into(),
            RunStatus::Running,
            None,
            None,
            None,
            None,
        ))
        .unwrap();
        lock_conn!(db).execute("INSERT INTO artifacts (id,run_id,flow_name,stage_name,attempt,version,content,content_hash,created_at)
            VALUES ('artifact-b','run-b','flow','stage',0,1,'{}','hash','now')", []).unwrap();
        assert!(!db.can_access_artifact("alice", "artifact-b").unwrap());
        assert!(!db.can_access_run("alice", "legacy").unwrap());
        assert!(db.can_access_run("admin", "legacy").unwrap());
        assert!(!db.can_access_artifact("admin", "missing").unwrap());
    }

    #[test]
    fn concurrent_bootstrap_only_creates_one_administrator() {
        let db = Arc::new(Database {
            conn: Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
        });
        db.migrate().unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|id| {
                let db = db.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    db.bootstrap_admin(&User {
                        id: id.into(),
                        username: id.into(),
                        password_hash: "unused".into(),
                        display_name: None,
                        is_admin: true,
                        created_at: String::new(),
                    })
                    .is_ok()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .filter_map(|w| w.join().ok())
                .filter(|won| *won)
                .count(),
            1
        );
        assert_eq!(db.count_users().unwrap(), 1);
    }

    use std::sync::Arc;
}

impl Database {
    pub fn bootstrap_admin(&self, user: &User) -> Result<()> {
        anyhow::ensure!(user.is_admin, "Bootstrap account must be an administrator");
        let conn = lock_conn!(self);
        let changed = conn.execute("INSERT INTO users (id,username,password_hash,display_name,is_admin,created_at,updated_at)
            SELECT ?1,?2,?3,?4,1,?5,?5 WHERE NOT EXISTS(SELECT 1 FROM users)",
            params![user.id,user.username,user.password_hash,user.display_name,chrono::Utc::now().to_rfc3339()])?;
        anyhow::ensure!(changed == 1, "Registration is closed");
        Ok(())
    }

    /// Re-read account rights; never trust a caller-supplied admin flag.
    pub fn is_admin_user(&self, user_id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1 AND is_admin = 1)",
            params![user_id],
            |row| row.get(0),
        )?)
    }

    pub fn can_access_project(&self, user_id: &str, project_id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM users u JOIN projects p ON p.id = ?2
            WHERE u.id = ?1 AND (u.is_admin = 1 OR EXISTS(SELECT 1 FROM project_members m
                WHERE m.project_id = p.id AND m.user_id = u.id)))",
            params![user_id, project_id],
            |row| row.get(0),
        )?)
    }

    /// Parentless legacy runs are visible only to instance administrators.
    pub fn can_access_run(&self, user_id: &str, run_id: &str) -> Result<bool> {
        let conn = lock_conn!(self);
        Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM users u JOIN pipeline_runs r ON r.id = ?2
            WHERE u.id = ?1 AND (u.is_admin = 1 OR EXISTS(SELECT 1 FROM project_members m
                JOIN projects p ON p.id = m.project_id WHERE m.project_id = r.project_id AND m.user_id = u.id)))",
            params![user_id, run_id], |row| row.get(0))?)
    }

    pub fn can_access_lane(&self, user_id: &str, lane_id: &str) -> Result<bool> {
        match self.load_lane(lane_id)? {
            Some(lane) => self.can_access_project(user_id, lane.project_id.as_str()),
            None => Ok(false),
        }
    }

    pub fn can_access_artifact(&self, user_id: &str, artifact_id: &str) -> Result<bool> {
        let run_id: Option<String> = {
            use rusqlite::OptionalExtension;
            let conn = lock_conn!(self);
            conn.query_row(
                "SELECT run_id FROM artifacts WHERE id = ?1",
                params![artifact_id],
                |row| row.get(0),
            )
            .optional()?
        };
        match run_id {
            Some(id) => self.can_access_run(user_id, &id),
            None => Ok(false),
        }
    }

    pub fn set_project_member(
        &self,
        actor_id: &str,
        project_id: &str,
        user_id: &str,
        member: bool,
    ) -> Result<()> {
        let mut conn = lock_conn!(self);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let permitted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1 AND is_admin = 1)",
            params![actor_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(permitted, "Administrator access required");
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects p JOIN users u ON u.id = ?2 WHERE p.id = ?1)",
            params![project_id, user_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(exists, "Project or user not found");
        if member {
            tx.execute(
                "INSERT OR IGNORE INTO project_members (project_id,user_id) VALUES (?1,?2)",
                params![project_id, user_id],
            )?;
        } else {
            tx.execute(
                "DELETE FROM project_members WHERE project_id = ?1 AND user_id = ?2",
                params![project_id, user_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn project_member_ids(&self, actor_id: &str, project_id: &str) -> Result<Vec<String>> {
        anyhow::ensure!(
            self.is_admin_user(actor_id)?,
            "Administrator access required"
        );
        let conn = lock_conn!(self);
        let mut stmt = conn.prepare(
            "SELECT user_id FROM project_members WHERE project_id = ?1 ORDER BY user_id",
        )?;
        Ok(stmt
            .query_map(params![project_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
}
