use eyre::{Context, Result};
use std::path::Path;

use git2::{build::CheckoutBuilder, FetchOptions, Repository, WorktreeAddOptions};

pub fn fetch_and_get_branches<'a>(
    base_sha: &str,
    head_sha: &str,
    repo: &'a git2::Repository,
    head_branch_name: &str,
    base_branch_name: &str,
) -> Result<(git2::Reference<'a>, git2::Reference<'a>)> {
    let base_id = git2::Oid::from_str(base_sha).context("Parsing base sha")?;
    let head_id = git2::Oid::from_str(head_sha).context("Parsing head sha")?;

    let mut remote = repo.find_remote("origin")?;

    remote
        .connect(git2::Direction::Fetch)
        .context("Connecting to remote")?;

    remote
        .fetch(
            &[base_branch_name],
            Some(FetchOptions::new().prune(git2::FetchPrune::On)),
            None,
        )
        .context("Fetching base")?;
    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .context("Getting FETCH_HEAD")?;

    let base_commit = repo
        .reference_to_annotated_commit(&fetch_head)
        .context("Getting commit from FETCH_HEAD")?;

    if let Some(branch) = repo
        .find_branch(base_branch_name, git2::BranchType::Local)
        .ok()
        .and_then(|branch| branch.is_head().then_some(branch))
    {
        branch
            .into_reference()
            .set_target(base_commit.id(), "Fast forwarding current ref")
            .context("Setting base reference to FETCH_HEAD's commit")?;
    } else {
        repo.branch_from_annotated_commit(base_branch_name, &base_commit, true)
            .context("Setting a new base branch to FETCH_HEAD's commit")?;
    }

    repo.set_head(
        repo.resolve_reference_from_short_name(base_branch_name)?
            .name()
            .unwrap(),
    )
    .context("Setting HEAD to base")?;

    let commit = match repo.find_commit(base_id).context("Finding base commit") {
        Ok(commit) => commit,
        Err(_) => repo.head()?.peel_to_commit()?,
    };

    repo.resolve_reference_from_short_name(base_branch_name)?
        .set_target(commit.id(), "Setting default branch to the correct commit")?;

    let base_branch = repo
        .resolve_reference_from_short_name(base_branch_name)
        .context("Getting the base reference")?;

    remote
        .fetch(
            &[head_branch_name],
            Some(FetchOptions::new().prune(git2::FetchPrune::On)),
            None,
        )
        .context("Fetching head")?;

    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .context("Getting FETCH_HEAD")?;

    let head_name = format!("mdb-pull-{base_sha}-{head_sha}");

    let mut head_branch = repo
        .branch_from_annotated_commit(
            &head_name,
            &repo.reference_to_annotated_commit(&fetch_head)?,
            true,
        )
        .context("Creating branch")?
        .into_reference();

    repo.set_head(head_branch.name().unwrap())
        .context("Setting HEAD to head")?;

    let head_commit = match repo.find_commit(head_id).context("Finding head commit") {
        Ok(commit) => commit,
        Err(_) => repo.head()?.peel_to_commit()?,
    };

    head_branch.set_target(
        head_commit.id(),
        "Setting head branch to the correct commit",
    )?;

    let head_branch = repo
        .resolve_reference_from_short_name(&head_name)
        .context("Getting the head reference")?;

    remote.disconnect().context("Disconnecting from remote")?;

    repo.set_head(
        repo.resolve_reference_from_short_name(base_branch_name)?
            .name()
            .unwrap(),
    )
    .context("Setting head to default branch")?;

    repo.checkout_head(Some(
        CheckoutBuilder::default()
            .force()
            .remove_ignored(true)
            .remove_untracked(true),
    ))
    .context("Resetting to base commit")?;

    Ok((base_branch, head_branch))
}

pub fn clean_up_references(repo: &Repository, branch: &str) -> Result<()> {
    repo.set_head(
        repo.resolve_reference_from_short_name(branch)?
            .name()
            .unwrap(),
    )
    .context("Setting head")?;
    repo.checkout_head(Some(
        CheckoutBuilder::new()
            .force()
            .remove_ignored(true)
            .remove_untracked(true),
    ))
    .context("Checkout to head")?;
    let mut references = repo.references().context("Getting all references")?;
    let references = references
        .names()
        .filter_map(move |reference| {
            (reference.as_ref().ok()?.contains("pull-"))
                .then(move || reference.ok())
                .flatten()
        })
        .map(|item| item.to_owned())
        .collect::<Vec<_>>();

    for refname in references {
        let mut reference = repo
            .find_reference(&refname)
            .context("Looking for ref to delete")?;
        reference.delete().context("Deleting reference")?;
    }
    Ok(())
}

pub fn with_checkout<T>(
    checkout_ref: &git2::Reference,
    repo: &Repository,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    repo.set_head(checkout_ref.name().unwrap())?;
    repo.checkout_head(Some(
        CheckoutBuilder::new()
            .force()
            .remove_ignored(true)
            .remove_untracked(true),
    ))?;
    f()
}

pub fn with_checkout_worktree<T>(
    checkout_ref: &git2::Reference,
    worktree_name: &str,
    repo: &Repository,
    f: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let worktree = if let Ok(worktree) = repo.find_worktree(worktree_name) {
        // Update the worktree
        let worktree_repo =
            Repository::open_from_worktree(&worktree).context("Opening worktree repo")?;
        worktree_repo.set_head(checkout_ref.name().unwrap())?;
        worktree_repo.checkout_head(Some(
            CheckoutBuilder::new()
                .force()
                .remove_ignored(true)
                .remove_untracked(true),
        ))?;
        worktree
    } else {
        // Worktree doesn't exist yet
        repo.worktree(
            worktree_name,
            &repo
                .workdir()
                // It's safe to assume we always have a working dir because we never clone bare repositories
                .unwrap()
                .join(worktree_name),
            Some(WorktreeAddOptions::new().reference(Some(checkout_ref))),
        )
        .context("Creating new worktree")?
    };

    f(worktree.path())
}

pub fn clone_repo(url: &str, dir: &Path) -> Result<()> {
    git2::Repository::clone(url, dir.as_os_str()).context("Cloning repo")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use path_absolutize::Absolutize;

    const BASE_SHA: &str = "9caa787ea37aa86e6203b84dbd2f8f9c1d29fbd7";
    const BASE_BRANCH: &str = "master";
    const HEAD_SHA: &str = "e1a57148d8113372d4bc865493394d74491be914";
    const HEAD_BRANCH: &str = "worktree_ref";

    #[test]
    // This is very expensive so we only want to run it when requested.
    #[ignore]
    fn test_worktree() {
        let tempdir = tempfile::tempdir().unwrap();

        // Drop everything before we try and close the tempdir
        {
            let path = tempdir.path();

            clone_repo(
                "https://github.com/spacestation13/BYONDDiffBotsTestRepo.git",
                path,
            )
            .expect("Failed to clone repository to temporary directory.");

            println!("Cloned repo to {:#?}", path.absolutize());

            let repo = Repository::open(path).expect("Failed to open repository");

            let (_base, worktree) =
                fetch_and_get_branches(BASE_SHA, HEAD_SHA, &repo, HEAD_BRANCH, BASE_BRANCH)
                    .expect("Failed to get refs");

            with_checkout_worktree(&worktree, "_mdb2_worktree_head", &repo, |path| {
                println!("Operating on worktree in {:#?}", path);
                assert!(path.exists());
                assert!(path.join("WORKTREE.md").exists());

                println!("Success: Confirmed worktree was correct");
                Ok(())
            })
            .expect("Failed to use with_checkout_worktree");

            assert!(!repo.workdir().unwrap().join("WORKTREE.md").exists());
            assert!(repo.workdir().unwrap().join("_mdb2_worktree_head").exists());
            println!("Success: Confirmed base tree was unaffected other than worktree folder");

            println!("Testing to make sure worktree works twice");

            with_checkout_worktree(&worktree, "_mdb2_worktree_head", &repo, |path| {
                println!("Operating on worktree in {:#?}", path);
                assert!(path.exists());
                assert!(path.join("WORKTREE.md").exists());

                println!("Success: Confirmed worktree was correct");
                Ok(())
            })
            .expect("Failed to use with_checkout_worktree");
        }

        tempdir
            .close()
            .expect("Failed to clean up temporary directory");
    }
}
