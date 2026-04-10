use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};

use crate::action::{Action, GithubDiscoverPayload};
use crate::background;
use crate::state::Modal;

use super::App;

impl App {
    pub(super) fn handle_discover_github_orgs(&mut self) {
        self.state.modal = Modal::GithubDiscoverOrgs {
            orgs: Vec::new(),
            cursor: 0,
            loading: true,
            error: None,
        };

        if let Some(ref tx) = self.bg_tx {
            let tx = tx.clone();
            background::spawn_blocking(
                tx,
                move || match conductor_core::github::list_github_orgs() {
                    Ok(orgs) => Action::GithubOrgsLoaded { orgs },
                    Err(e) => Action::GithubOrgsFailed {
                        error: e.to_string(),
                    },
                },
            );
        }
    }

    pub(super) fn handle_github_orgs_loaded(&mut self, orgs: Vec<String>) {
        if !matches!(
            self.state.modal,
            Modal::GithubDiscoverOrgs { loading: true, .. }
        ) {
            return;
        }
        // Prepend empty string sentinel for "Personal" (displayed as "Personal")
        let mut display_orgs = vec![String::new()];
        display_orgs.extend(orgs);
        self.state.github_orgs_cache = display_orgs.clone();
        self.state.modal = Modal::GithubDiscoverOrgs {
            orgs: display_orgs,
            cursor: 0,
            loading: false,
            error: None,
        };
    }

    pub(super) fn handle_github_orgs_failed(&mut self, error: String) {
        if matches!(self.state.modal, Modal::GithubDiscoverOrgs { .. }) {
            self.state.modal = Modal::GithubDiscoverOrgs {
                orgs: Vec::new(),
                cursor: 0,
                loading: false,
                error: Some(error),
            };
        }
    }

    pub(super) fn handle_github_drill_into_owner(&mut self, owner: String) {
        let registered_urls: Vec<String> = self
            .state
            .data
            .repos
            .iter()
            .map(|r| r.remote_url.clone())
            .collect();

        let owner_opt = if owner.is_empty() {
            None
        } else {
            Some(owner.clone())
        };

        self.state.modal = Modal::GithubDiscover {
            owner: owner.clone(),
            repos: Vec::new(),
            registered_urls: Vec::new(),
            selected: Vec::new(),
            cursor: 0,
            loading: true,
            error: None,
        };

        if let Some(ref tx) = self.bg_tx {
            let tx = tx.clone();
            background::spawn_blocking(tx, move || {
                match conductor_core::github::discover_github_repos(owner_opt.as_deref()) {
                    Ok(repos) => Action::GithubDiscoverLoaded(Box::new(GithubDiscoverPayload {
                        owner: owner_opt.unwrap_or_default(),
                        repos,
                        registered_urls,
                    })),
                    Err(e) => Action::GithubDiscoverFailed {
                        error: e.to_string(),
                    },
                }
            });
        }
    }

    pub(super) fn handle_github_back_to_orgs(&mut self) {
        let orgs = self.state.github_orgs_cache.clone();
        self.state.modal = Modal::GithubDiscoverOrgs {
            orgs,
            cursor: 0,
            loading: false,
            error: None,
        };
    }

    pub(super) fn handle_github_discover_loaded(&mut self, payload: GithubDiscoverPayload) {
        // Only update if the modal is still open in loading state
        if let Modal::GithubDiscover { loading, .. } = self.state.modal {
            if !loading {
                return;
            }
        } else {
            return;
        }

        let count = payload.repos.len();
        self.state.modal = Modal::GithubDiscover {
            owner: payload.owner,
            selected: vec![false; count],
            repos: payload.repos,
            registered_urls: payload.registered_urls,
            cursor: 0,
            loading: false,
            error: None,
        };
    }

    pub(super) fn handle_github_discover_failed(&mut self, error: String) {
        let owner = if let Modal::GithubDiscover { ref owner, .. } = self.state.modal {
            owner.clone()
        } else {
            return;
        };
        self.state.modal = Modal::GithubDiscover {
            owner,
            repos: Vec::new(),
            registered_urls: Vec::new(),
            selected: Vec::new(),
            cursor: 0,
            loading: false,
            error: Some(error),
        };
    }

    pub(super) fn handle_github_discover_toggle(&mut self) {
        if let Modal::GithubDiscover {
            ref repos,
            ref registered_urls,
            ref mut selected,
            cursor,
            ..
        } = self.state.modal
        {
            if let Some(sel) = selected.get_mut(cursor) {
                let repo = &repos[cursor];
                let is_registered = registered_urls.contains(&repo.clone_url)
                    || registered_urls.contains(&repo.ssh_url);
                if !is_registered {
                    *sel = !*sel;
                }
            }
        }
    }

    pub(super) fn handle_github_discover_select_all(&mut self) {
        if let Modal::GithubDiscover {
            ref repos,
            ref registered_urls,
            ref mut selected,
            ..
        } = self.state.modal
        {
            let any_unselected = repos.iter().zip(selected.iter()).any(|(r, &s)| {
                !s && !registered_urls.contains(&r.clone_url)
                    && !registered_urls.contains(&r.ssh_url)
            });
            for (repo, sel) in repos.iter().zip(selected.iter_mut()) {
                let is_registered = registered_urls.contains(&repo.clone_url)
                    || registered_urls.contains(&repo.ssh_url);
                if !is_registered {
                    *sel = any_unselected;
                }
            }
        }
    }

    pub(super) fn handle_github_discover_import(&mut self) {
        let to_import: Vec<String> = if let Modal::GithubDiscover {
            ref repos,
            ref registered_urls,
            ref selected,
            ..
        } = self.state.modal
        {
            repos
                .iter()
                .zip(selected.iter())
                .filter_map(|(repo, &sel)| {
                    if sel
                        && !registered_urls.contains(&repo.clone_url)
                        && !registered_urls.contains(&repo.ssh_url)
                    {
                        Some(repo.clone_url.clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            return;
        };

        if to_import.is_empty() {
            return;
        }

        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: "Cannot import repos: background sender not ready.".into(),
            };
            return;
        };
        self.state.modal = Modal::Progress {
            message: "Importing repos…".to_string(),
        };
        let config = self.config.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<(usize, Vec<String>)> {
                let db = conductor_core::config::db_path();
                let conn = conductor_core::db::open_database(&db)?;
                let mgr = RepoManager::new(&conn, &config);
                let mut imported = 0usize;
                let mut errors = Vec::new();
                for url in &to_import {
                    let slug = derive_slug_from_url(url);
                    let local_path = derive_local_path(&config, &slug);
                    match mgr.register(&slug, &local_path, url, None) {
                        Ok(_) => imported += 1,
                        Err(e) => errors.push(format!("{slug}: {e}")),
                    }
                }
                Ok((imported, errors))
            })();
            match result {
                Ok((imported, errors)) => {
                    let _ = bg_tx.send(Action::GithubImportComplete { imported, errors });
                }
                Err(e) => {
                    let _ = bg_tx.send(Action::GithubImportComplete {
                        imported: 0,
                        errors: vec![e.to_string()],
                    });
                }
            }
        });
    }
}
