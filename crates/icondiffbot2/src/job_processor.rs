// should fix but lazy
#![allow(clippy::format_push_string)]

use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::Path,
    sync::Arc,
};

use anyhow::{format_err, Context, Result};
use diffbot_lib::{
    github::{
        github_api::download_url,
        github_types::{CheckOutputs, ModifiedFileStatus, Output},
    },
    job::types::Job,
};
use dmm_tools::dmi::render::IconRenderer;
use dmm_tools::dmi::{IconFile, State};
use tokio::{runtime::Handle, sync::Mutex};

use crate::CONFIG;

pub fn do_job(job: &Job) -> Result<CheckOutputs> {
    // TODO: Maybe have jobs just be async?
    let handle = Handle::try_current()?;
    handle.block_on(async { handle_changed_files(job).await })
}

fn status_to_sha(job: &Job, status: ModifiedFileStatus) -> (Option<&str>, Option<&str>) {
    match status {
        ModifiedFileStatus::Added => (None, Some(&job.head.sha)),
        ModifiedFileStatus::Removed => (Some(&job.base.sha), None),
        ModifiedFileStatus::Modified => (Some(&job.base.sha), Some(&job.head.sha)),
        ModifiedFileStatus::Renamed => (None, None),
        ModifiedFileStatus::Copied => (None, None),
        ModifiedFileStatus::Changed => (None, None), // TODO: look up what this is
        ModifiedFileStatus::Unchanged => (None, None),
    }
}

struct IconFileWithName {
    pub full_name: String,
    pub sha: String,
    pub hash: u64,
    pub icon: IconFile,
}

async fn get_if_exists(
    job: &Job,
    filename: &str,
    sha: Option<&str>,
) -> Result<Option<IconFileWithName>> {
    if let Some(sha) = sha {
        let raw = download_url(&job.installation, &job.base.repo, filename, sha)
            .await
            .with_context(|| format!("Failed to download file {:?}", filename))?;

        let mut hasher = DefaultHasher::new();
        raw.hash(&mut hasher);
        let hash = hasher.finish();

        Ok(Some(IconFileWithName {
            full_name: filename.to_string(),
            sha: sha.to_string(),
            hash,
            icon: IconFile::from_raw(raw)
                .with_context(|| format!("IconFile::from_raw failed for {:?}", filename))?,
        }))
    } else {
        Ok(None)
    }
}

async fn sha_to_iconfile(
    job: &Job,
    filename: &str,
    sha: (Option<&str>, Option<&str>),
) -> Result<(Option<IconFileWithName>, Option<IconFileWithName>)> {
    Ok((
        get_if_exists(job, filename, sha.0).await?,
        get_if_exists(job, filename, sha.1).await?,
    ))
}

pub async fn handle_changed_files(job: &Job) -> Result<CheckOutputs> {
    job.check_run.mark_started().await?;
    // TODO: tempted to use an <img> tag so i can set a style that upscales 32x32 to 64x64 and sets all the browser flags for nearest neighbor scaling

    let protected_job = Arc::new(Mutex::new(job));

    let mut map = HashMap::new();

    for dmi in &job.files {
        let states = render(
            Arc::clone(&protected_job),
            sha_to_iconfile(job, &dmi.filename, status_to_sha(job, dmi.status)).await?,
        )
        .await?;
        map.insert(dmi.filename.as_str(), states);
    }

    let mut file_names: HashMap<&str, u32> = HashMap::new();
    let mut details: Vec<(String, &str, String)> = Vec::new();
    let mut current_table = String::new();

    for (file_name, (change_type, states)) in map.iter() {
        let entry = file_names.entry(file_name).or_insert(0);

        for state in states {
            // A little extra buffer room for the <detail> block
            if current_table.len() + state.len() > 55_000 {
                details.push((
                    format!("{} ({})", file_name, *entry),
                    change_type,
                    std::mem::take(&mut current_table),
                ));
                *entry += 1;
            }
            current_table.push_str(state.as_str());
            current_table.push('\n');
        }

        if !current_table.is_empty() {
            details.push((
                format!("{} ({})", file_name, *entry),
                change_type,
                std::mem::take(&mut current_table),
            ));
            *entry += 1;
        }
    }

    let mut chunks: Vec<Output> = Vec::new();
    let mut current_output_text = String::new();

    for (file_name, change_type, table) in details.iter() {
        let diff_block = format!(
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/templates/diff_details.txt"
            )),
            filename = file_name,
            table = table,
            typ = change_type,
        );

        if current_output_text.len() + diff_block.len() > 60_000 {
            chunks.push(Output {
                title: "Icon difference rendering".to_owned(),
                summary: "*This is still a beta. Please file any issues [here](https://github.com/spacestation13/BYONDDiffBots/).*\n\nIcons with diff:".to_owned(),
                text: std::mem::take(&mut current_output_text)
            });
        }

        current_output_text.push_str(&diff_block);
    }

    if !current_output_text.is_empty() {
        chunks.push(Output {
            title: "Icon difference rendering".to_owned(),
            summary: "*This is still a beta. Please file any issues [here](https://github.com/spacestation13/BYONDDiffBots/).*\n\nIcons with diff:".to_owned(),
            text: std::mem::take(&mut current_output_text)
        });
    }

    let first = chunks.drain(0..1).next().unwrap();
    if !chunks.is_empty() {
        Ok(CheckOutputs::Many(first, chunks))
    } else {
        Ok(CheckOutputs::One(first))
    }
}

async fn render(
    job: Arc<Mutex<&Job>>,
    diff: (Option<IconFileWithName>, Option<IconFileWithName>),
) -> Result<(String, Vec<String>)> {
    // TODO: Alphabetize
    // TODO: Test more edge cases
    // TODO: Parallelize?
    match diff {
        (None, None) => unreachable!("Diffing (None, None) makes no sense"),
        (None, Some(after)) => {
            let urls = full_render(job, &after)
                .await
                .context("Failed to render new icon file")?;
            let mut builder = Vec::new();
            for url in urls {
                let mut state_name = url.0;
                // Mark default states
                if state_name.is_empty() {
                    state_name = "{{DEFAULT}}".to_string();
                }

                builder.push(format!(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/templates/diff_line.txt"
                    )),
                    state_name = state_name,
                    old = "",
                    new = url.1,
                    change_text = "Created",
                ));
            }

            Ok(("ADDED".to_owned(), builder))
        }
        (Some(before), None) => {
            // dbg!(&before.icon.metadata);
            let urls = full_render(job, &before)
                .await
                .context("Failed to render deleted icon file")?;
            // dbg!(&urls);
            let mut builder = Vec::new();
            for url in urls {
                let mut state_name = url.0;
                // Mark default states
                if state_name.is_empty() {
                    state_name = "{{DEFAULT}}".to_string();
                }

                // Build the output line
                builder.push(format!(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/templates/diff_line.txt"
                    )),
                    state_name = state_name,
                    old = url.1,
                    new = "",
                    change_text = "Deleted",
                ));
            }

            Ok(("DELETED".to_owned(), builder))
        }
        (Some(before), Some(after)) => {
            let before_states: HashSet<String> =
                before.icon.metadata.state_names.keys().cloned().collect();
            let after_states: HashSet<String> =
                after.icon.metadata.state_names.keys().cloned().collect();

            let access = job.lock().await;
            let prefix = format!("{}/{}", access.installation, access.pull_request);
            drop(access);

            let mut builder = Vec::new();
            let mut before_renderer = IconRenderer::new(&before.icon);
            let mut after_renderer = IconRenderer::new(&after.icon);

            for state in before_states.symmetric_difference(&after_states) {
                if before_states.contains(state) {
                    let (name, url) = render_state(
                        &prefix,
                        &before,
                        before.icon.metadata.get_icon_state(state).unwrap(),
                        &mut before_renderer,
                    )
                    .await
                    .with_context(|| format!("Failed to render before-state {state}"))?;
                    builder.push(format!(
                        include_str!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/templates/diff_line.txt"
                        )),
                        state_name = name,
                        old = url,
                        new = "",
                        change_text = "Deleted",
                    ));
                } else {
                    let (name, url) = render_state(
                        &prefix,
                        &after,
                        after.icon.metadata.get_icon_state(state).unwrap(),
                        &mut after_renderer,
                    )
                    .await
                    .with_context(|| format!("Failed to render after-state {state}"))?;
                    builder.push(format!(
                        include_str!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/templates/diff_line.txt"
                        )),
                        state_name = name,
                        old = "",
                        new = url,
                        change_text = "Created",
                    ));
                }
            }

            for state in before_states.intersection(&after_states) {
                let before_state = before.icon.metadata.get_icon_state(state).unwrap();
                let after_state = after.icon.metadata.get_icon_state(state).unwrap();

                let difference = {
                    // #[cfg(debug_assertions)]
                    // dbg!(before_state, after_state);
                    if before_state != after_state {
                        true
                    } else {
                        let before_state_render = before_renderer.render_to_images(state)?;
                        let after_state_render = after_renderer.render_to_images(state)?;
                        before_state_render != after_state_render
                    }
                };

                if difference {
                    let before_state = before.icon.metadata.get_icon_state(state).unwrap();
                    let after_state = after.icon.metadata.get_icon_state(state).unwrap();

                    let (_, before_url) =
                        render_state(&prefix, &before, before_state, &mut before_renderer)
                            .await
                            .with_context(|| {
                                format!("Failed to render modified before-state {state}")
                            })?;
                    let (_, after_url) =
                        render_state(&prefix, &after, after_state, &mut after_renderer)
                            .await
                            .with_context(|| {
                                format!("Failed to render modified before-state {state}")
                            })?;

                    builder.push(format!(
                        include_str!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/templates/diff_line.txt"
                        )),
                        state_name = state,
                        old = before_url,
                        new = after_url,
                        change_text = "Modified",
                    ));
                }
                /* else {
                    println!("No difference detected for {}", state);
                } */
            }

            Ok(("MODIFIED".to_owned(), builder))
        }
    }
}

async fn render_state<'a, S: AsRef<str>>(
    prefix: S,
    target: &IconFileWithName,
    state: &State,
    renderer: &mut IconRenderer<'a>,
) -> Result<(String, String)> {
    let directory = Path::new(".").join("images").join(prefix.as_ref());
    // Always remember to mkdir -p your paths
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("Failed to create directory {:?}", directory))?;

    let mut hasher = DefaultHasher::new();
    target.sha.hash(&mut hasher);
    target.full_name.hash(&mut hasher);
    target.hash.hash(&mut hasher);
    state.duplicate.unwrap_or(0).hash(&mut hasher);
    state.name.hash(&mut hasher);
    let filename = hasher.finish().to_string();

    // TODO: Calculate file extension separately so that we can Error here if we overwrite a file
    let path = directory.join(&filename);
    // dbg!(&path, &state.frames);
    let corrected_path = renderer
        .render_state(state, &path)
        .with_context(|| format!("Failed to render state {} to file {:?}", state.name, path))?;
    let extension = corrected_path
        .extension()
        .ok_or_else(|| format_err!("Unable to get extension that was written to"))?;
    // dbg!(&corrected_path, &extension);

    let url = format!(
        "{}/{}/{}.{}",
        CONFIG.get().unwrap().file_hosting_url,
        prefix.as_ref(),
        filename,
        extension.to_string_lossy()
    );

    Ok((state.get_state_name_index(), url))
}

async fn full_render(
    job: Arc<Mutex<&Job>>,
    target: &IconFileWithName,
) -> Result<Vec<(String, String)>> {
    let icon = &target.icon;

    let mut vec = Vec::new();

    let mut renderer = IconRenderer::new(icon);

    let access = job.lock().await;
    let prefix = format!("{}/{}", access.installation, access.pull_request);
    drop(access);

    for state in icon.metadata.states.iter() {
        let (name, url) = render_state(&prefix, target, state, &mut renderer)
            .await
            .with_context(|| format!("Failed to render state {}", state.name))?;
        vec.push((name, url));
    }

    // dbg!(&vec);

    Ok(vec)
}
