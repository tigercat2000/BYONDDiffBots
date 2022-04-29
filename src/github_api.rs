use crate::{github_types::*, job::Job};
use anyhow::{Context, Result};

pub async fn update_check_run(job: &Job, builder: UpdateCheckRunBuilder) -> Result<()> {
    let check_run = builder.build().context("Building UpdateCheckRun")?;

    let _: Empty = octocrab::instance()
        .installation(job.installation_id.into())
        .patch(
            format!(
                "/repos/{repo}/check-runs/{check_run_id}",
                repo = job.base.repo.full_name(),
                check_run_id = job.check_run_id
            ),
            Some(&check_run),
        )
        .await
        .context("Getting files")?;

    Ok(())
}

pub async fn get_pull_files(
    installation: &Installation,
    pull: &PullRequest,
) -> Result<Vec<ModifiedFile>> {
    let res = octocrab::instance()
        .installation(installation.id.into())
        .get(
            &format!(
                "/repos/{repo}/pulls/{pull_number}/files",
                repo = pull.base.repo.full_name(),
                pull_number = pull.number
            ),
            None::<&()>,
        )
        .await?;

    Ok(res)
}

pub async fn submit_check(full_repo: &str, head_sha: &str, inst_id: u64) -> Result<CheckRun> {
    let result: CheckRun = octocrab::instance()
        .installation(inst_id.into())
        .post(
            format!("/repos/{full_repo}/check-runs"),
            Some(&CreateCheckRun {
                name: "MapDiffBot2".to_string(),
                head_sha: head_sha.to_string(),
            }),
        )
        .await
        .context("Submitting check")?;

    Ok(result)
}

pub async fn mark_job_queued(job: &Job) -> Result<()> {
    update_check_run(
        job,
        UpdateCheckRunBuilder::default()
            .status("queued")
            .started_at(chrono::Utc::now().to_rfc3339()),
    )
    .await
    .context("Marking check run as queued")
}

pub async fn mark_job_started(job: &Job) -> Result<()> {
    update_check_run(
        job,
        UpdateCheckRunBuilder::default()
            .status("in_progress")
            .started_at(chrono::Utc::now().to_rfc3339()),
    )
    .await
    .context("Marking check as in progress")
}

pub async fn update_job(job: &Job, output: Output) -> Result<()> {
    update_check_run(job, UpdateCheckRunBuilder::default().output(output))
        .await
        .context("Updating check")
}

pub async fn mark_job_failed(job: &Job, error: &str) -> Result<()> {
    let summary = format!(include_str!("error_template.txt"), stack_trace = error);

    update_check_run(
        job,
        UpdateCheckRunBuilder::default()
            .status("completed")
            .conclusion("failure")
            .completed_at(chrono::Utc::now().to_rfc3339())
            .output(Output {
                title: "Error handling job".to_owned(),
                summary,
                text: "".to_owned(),
            }),
    )
    .await
    .context("Marking check as failure")
}

pub async fn mark_job_success(job: &Job, output: Output) -> Result<()> {
    update_check_run(
        job,
        UpdateCheckRunBuilder::default()
            .conclusion("success")
            .completed_at(chrono::Utc::now().to_rfc3339())
            .output(output),
    )
    .await
    .context("Marking check as success")
}

pub async fn mark_job_skipped(job: &Job, output: Output) -> Result<()> {
    update_check_run(
        job,
        UpdateCheckRunBuilder::default()
            .conclusion("skipped")
            .completed_at(chrono::Utc::now().to_rfc3339())
            .output(output),
    )
    .await
    .context("Marking check as skipped")
}
