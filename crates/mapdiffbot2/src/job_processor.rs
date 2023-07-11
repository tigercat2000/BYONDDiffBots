use diffbot_lib::log;
use eyre::{Context, Result};
use path_absolutize::Absolutize;
use rayon::prelude::*;
use std::path::Path;
use std::path::PathBuf;

use super::git_operations::{
    clean_up_references, clone_repo, fetch_and_get_branches, with_checkout,
};

use crate::rendering::{
    get_map_diff_bounding_boxes, load_maps, load_maps_with_whole_map_regions,
    render_diffs_for_directory, render_map_regions, MapWithRegions, MapsWithRegions,
    RenderingContext,
};

use crate::CONFIG;

use diffbot_lib::{
    github::github_types::{
        Branch, ChangeType, CheckOutputBuilder, CheckOutputs, FileDiff, Output,
    },
    job::types::Job,
};

use super::Azure;

struct RenderedMaps {
    added_maps: Vec<(String, MapWithRegions)>,
    removed_maps: Vec<(String, MapWithRegions)>,
    modified_maps: MapsWithRegions,
}

fn render(
    base: &Branch,
    head: &Branch,
    (added_files, modified_files, removed_files): (&[&FileDiff], &[&FileDiff], &[&FileDiff]),
    (repo, base_branch_name): (&git2::Repository, &str),
    (repo_dir, out_dir, blob_client): (&Path, &Path, Azure),
    pull_request_number: u64,
    // feel like this is a bit of a hack but it works for now
) -> Result<RenderedMaps> {
    log::debug!(
        "Fetching and getting branches, base: {:?}, head: {:?}",
        base,
        head
    );

    let pull_branch = format!("mdb-{}-{}", base.sha, head.sha);
    let head_branch = format!("pull/{pull_request_number}/head:{pull_branch}");

    let (base_branch, head_branch) =
        fetch_and_get_branches(&base.sha, &head.sha, repo, &head_branch, base_branch_name)
            .context("Fetching and constructing diffs")?;

    let path = repo_dir.absolutize().context("Making repo path absolute")?;

    let base_context = with_checkout(&base_branch, repo, || RenderingContext::new(&path))
        .context("Parsing base")?;

    let head_context = with_checkout(&head_branch, repo, || RenderingContext::new(&path))
        .context("Parsing head")?;

    let base_render_passes = dmm_tools::render_passes::configure(
        base_context.map_config(),
        "",
        "hide-space,hide-invisible,random",
    );

    let head_render_passes = dmm_tools::render_passes::configure(
        head_context.map_config(),
        "",
        "hide-space,hide-invisible,random",
    );

    //do removed maps
    let mut removed_directory = out_dir.to_path_buf();
    removed_directory.push("r");
    let removed_directory = removed_directory.as_path();

    let removed_errors = Default::default();

    let removed_maps = with_checkout(&base_branch, repo, || {
        let maps = load_maps_with_whole_map_regions(removed_files, &path)
            .context("Loading removed maps")?;
        render_map_regions(
            &base_context,
            maps.iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect::<Vec<_>>()
                .as_slice(),
            &base_render_passes,
            (removed_directory, blob_client.clone()),
            "removed.png",
            &removed_errors,
            crate::rendering::MapType::Base,
        )
        .context("Rendering removed maps")?;
        Ok(maps)
    })?;

    //do added maps
    let mut added_directory = out_dir.to_path_buf();
    added_directory.push("a");
    let added_directory = added_directory.as_path();

    let added_errors = Default::default();

    let added_maps = with_checkout(&head_branch, repo, || {
        let maps =
            load_maps_with_whole_map_regions(added_files, &path).context("Loading added maps")?;
        render_map_regions(
            &head_context,
            maps.iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect::<Vec<_>>()
                .as_slice(),
            &head_render_passes,
            (added_directory, blob_client.clone()),
            "added.png",
            &added_errors,
            crate::rendering::MapType::Head,
        )
        .context("Rendering added maps")?;
        Ok(maps)
    })
    .context("Rendering modified after and added maps")?;

    //do modified maps
    let base_maps = with_checkout(&base_branch, repo, || Ok(load_maps(modified_files, &path)))
        .context("Loading base maps")?;
    let mut head_maps = with_checkout(&head_branch, repo, || Ok(load_maps(modified_files, &path)))
        .context("Loading head maps")?;

    let modified_maps = base_maps
        .into_iter()
        .map(|(k, v)| {
            (
                k.clone(),
                (
                    v,
                    head_maps.remove(&k).expect(
                        "head maps has maps that isn't inside base maps on modified comparison",
                    ),
                ),
            )
        })
        .collect::<indexmap::IndexMap<_, _, ahash::RandomState>>();

    if !head_maps.is_empty() {
        return Err(eyre::eyre!(
            "Did not account for the following maps in head_maps (this shouldn't happen): {:?}",
            head_maps.keys().collect::<Vec<_>>()
        ));
    }

    let modified_maps = get_map_diff_bounding_boxes(modified_maps)?;

    let mut modified_directory = out_dir.to_path_buf();
    modified_directory.push("m");
    let modified_directory = modified_directory.as_path();

    let modified_before_errors = Default::default();
    let modified_after_errors = Default::default();

    with_checkout(&base_branch, repo, || {
        render_map_regions(
            &base_context,
            modified_maps
                .iter()
                .filter_map(|(map_name, (before, _))| {
                    Some((map_name.as_str(), before.as_ref().ok()?))
                })
                .collect::<Vec<_>>()
                .as_slice(),
            &head_render_passes,
            (modified_directory, blob_client.clone()),
            "before.png",
            &modified_before_errors,
            crate::rendering::MapType::Base,
        )
        .context("Rendering modified before maps")?;
        Ok(())
    })?;

    with_checkout(&head_branch, repo, || {
        render_map_regions(
            &head_context,
            modified_maps
                .iter()
                .filter_map(|(map_name, (_, after))| Some((map_name.as_str(), after.as_ref()?)))
                .collect::<Vec<_>>()
                .as_slice(),
            &head_render_passes,
            (modified_directory, blob_client.clone()),
            "after.png",
            &modified_after_errors,
            crate::rendering::MapType::Head,
        )
        .context("Rendering modified after maps")?;
        Ok(())
    })?;

    (0..modified_files.len()).into_par_iter().for_each(|i| {
        render_diffs_for_directory(modified_directory.join(i.to_string()));
    });

    Ok(RenderedMaps {
        added_maps,
        modified_maps,
        removed_maps,
    })
}

fn generate_finished_output<P: AsRef<Path>>(
    file_directory: &P,
    maps: RenderedMaps,
) -> Result<CheckOutputs> {
    let conf = CONFIG.get().unwrap();
    let file_url = if conf.azure_blobs.is_some() {
        format!(
            "https://{}.blob.core.windows.net/{}",
            conf.azure_blobs.as_ref().unwrap().storage_account,
            conf.azure_blobs.as_ref().unwrap().storage_container
        )
    } else {
        conf.web.file_hosting_url.to_string()
    };
    let non_abs_directory = file_directory.as_ref().to_string_lossy();

    let mut builder = CheckOutputBuilder::new(
    "Map renderings",
    "*Please file any issues [here](https://github.com/spacestation13/BYONDDiffBots/issues).*\n\n*Github may fail to render some images, appearing as cropped on large map changes. Please use the raw links in this case.*\n\nMaps with diff:",
    );

    let link_base = format!("{file_url}/{non_abs_directory}");

    // Those are CPU bound but parallelizing would require builder to be thread safe and it's probably not worth the overhead
    maps.added_maps.iter().for_each(|(file, map)| {
        let file_index = file.clone().replace('/', "_").replace(".dmm", "");
        map.iter_levels().for_each(|(level, _)| {
            let link = format!("{link_base}/a/{file_index}/{level}-added.png");
            let name = format!("{} (Z-level: {})", file, level + 1);

            builder.add_text(&format!(
                include_str!("../templates/diff_template_add.txt"),
                filename = name,
                image_link = link
            ));
        });
    });

    maps.removed_maps.iter().for_each(|(file, map)| {
        let file_index = file.clone().replace('/', "_").replace(".dmm", "");
        map.iter_levels().for_each(|(level, _)| {
            let link = format!("{link_base}/r/{file_index}/{level}-removed.png");
            let name = format!("{} (Z-level: {})", file, level + 1);

            builder.add_text(&format!(
                include_str!("../templates/diff_template_remove.txt"),
                filename = name,
                image_link = link
            ));
        });
    });

    const Z_DELETED_TEXT: &str = "Z-LEVEL DELETED";
    const Z_ADDED_TEXT: &str = "Z-LEVEL ADDED";
    const ROW_DESC: &str = "If the image doesn't load, use the raw link above";

    maps.modified_maps
        .iter()
        .for_each(|(file, (before, _))| match before {
            Ok(map) => {
                let file_index = file.clone().replace('/', "_").replace(".dmm", "");
                map.iter_levels().for_each(|(level, region)| {
                    let link = format!("{link_base}/m/{file_index}/{level}");
                    let name = format!("{} (Z-level: {})", file, level + 1);
                    let (dim_x, dim_y, _) = map.map.dim_xyz();
                    let fmt_dim = format!("({}, {}, {})", dim_x, dim_y, level + 1);

                    let (link_before, link_after, link_diff) = (
                        format!("{link}-before.png"),
                        format!("{link}-after.png"),
                        format!("{link}-diff.png"),
                    );

                    match region {
                        crate::rendering::BoundType::None => (),
                        crate::rendering::BoundType::OnlyHead => {
                            #[allow(clippy::format_in_format_args)]
                            builder.add_text(&format!(
                                include_str!("../templates/diff_template_mod.txt"),
                                bounds = fmt_dim,
                                filename = name,
                                image_before_link = "Unavailable",
                                image_after_link = format!("[New]({link_after})"),
                                image_diff_link = "Unavailable",
                                old_row = Z_ADDED_TEXT,
                                new_row = format!("![{ROW_DESC}]({link_after})"),
                                diff_row = Z_ADDED_TEXT
                            ));
                        }
                        crate::rendering::BoundType::OnlyBase => {
                            #[allow(clippy::format_in_format_args)]
                            builder.add_text(&format!(
                                include_str!("../templates/diff_template_mod.txt"),
                                bounds = fmt_dim,
                                filename = name,
                                image_before_link = "Unavailable",
                                image_after_link = "Unavailable",
                                image_diff_link = "Unavailable",
                                old_row = Z_DELETED_TEXT,
                                new_row = Z_DELETED_TEXT,
                                diff_row = Z_DELETED_TEXT
                            ));
                        }
                        crate::rendering::BoundType::Both(bounds) => {
                            #[allow(clippy::format_in_format_args)]
                            builder.add_text(&format!(
                                include_str!("../templates/diff_template_mod.txt"),
                                bounds = bounds.to_string(),
                                filename = name,
                                image_before_link = format!("[Old]({link_before})"),
                                image_after_link = format!("[New]({link_after})"),
                                image_diff_link = format!("[Diff]({link_diff})"),
                                old_row = format!("![{ROW_DESC}]({link_before})"),
                                new_row = format!("![{ROW_DESC}]({link_after})"),
                                diff_row = format!("![{ROW_DESC}]({link_diff})")
                            ));
                        }
                    }
                });
            }
            Err(e) => {
                let error = format!("{e:?}");
                builder.add_text(&format!(
                    include_str!("../templates/diff_template_error.txt"),
                    filename = file,
                    error = error,
                ));
            }
        });

    Ok(builder.build())
}

pub fn do_job(job: Job, blob_client: Azure) -> Result<CheckOutputs> {
    log::debug!(
        "Starting Job on repo: {}, pr number: {}, base commit: {}, head commit: {}",
        job.repo.full_name(),
        job.pull_request,
        job.base.sha,
        job.head.sha
    );

    let base = &job.base;
    let head = &job.head;
    let repo = format!("https://github.com/{}", job.repo.full_name());
    let repo_dir: PathBuf = ["./repos/", &job.repo.full_name()].iter().collect();

    let handle = actix_web::rt::Runtime::new()?;

    if !repo_dir.exists() {
        log::debug!("Directory {:?} doesn't exist, creating dir", repo_dir);
        std::fs::create_dir_all(&repo_dir)?;
        handle.block_on(async {
                let output = Output {
                    title: "Cloning repo...",
                    summary: "The repository is being cloned, this will take a few minutes. Future runs will not require cloning.".to_owned(),
                    text: "".to_owned(),
                };
                if let Some(check_run) = &job.check_run {
                    let _ = check_run.set_output(output).await; // we don't really care if updating the job fails, just continue
                }
            });
        clone_repo(&repo, &repo_dir).context("Cloning repo")?;
    }

    let non_abs_directory = if let Some(check_run) = &job.check_run {
        format!("images/{}/{}", job.repo.id, check_run.id())
    } else {
        format!("images/{}/TEST", job.repo.id)
    };

    let output_directory = Path::new(&non_abs_directory)
        .absolutize()
        .context("Absolutizing images path")?;
    let output_directory = output_directory
        .as_ref()
        .to_str()
        .ok_or_else(|| eyre::anyhow!("Failed to create absolute path to image directory",))?;

    log::debug!(
        "Dirs absolutized from {:?} to {:?}",
        non_abs_directory,
        output_directory
    );

    let filter_on_status = |status: ChangeType| {
        job.files
            .iter()
            .filter(|f| f.status == status)
            .collect::<Vec<&FileDiff>>()
    };

    let added_files = filter_on_status(ChangeType::Added);
    let modified_files = filter_on_status(ChangeType::Modified);
    let removed_files = filter_on_status(ChangeType::Deleted);

    let repository = git2::Repository::open(&repo_dir).context("Opening repository")?;

    let mut remote = repository.find_remote("origin")?;

    remote
        .connect(git2::Direction::Fetch)
        .context("Connecting to remote")?;

    remote.disconnect().context("Disconnecting from remote")?;

    let output_directory = if blob_client.is_some() {
        Path::new(&non_abs_directory)
    } else {
        Path::new(output_directory)
    };

    let res = match render(
        base,
        head,
        (&added_files, &modified_files, &removed_files),
        (&repository, &job.base.r#ref),
        (&repo_dir, output_directory, blob_client),
        job.pull_request,
    ) {
        Ok(maps) => generate_finished_output(&non_abs_directory, maps),
        Err(err) => Err(err),
    };

    clean_up_references(&repository, &job.base.r#ref).context("Cleaning up references")?;

    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffbot_lib::github::github_types;
    use octocrab::models::InstallationId;
    use tempfile::tempdir;

    #[test]
    #[ignore]
    fn test_tgstation_75440() {
        const BASE: &str = "df8ba3d90e337ba55af7783dc759ab0b29158439";
        const BASE_REF: &str = "master";
        const HEAD: &str = "092c11b3158579b7731265f61cbd7cca9d2f4587";
        const HEAD_REF: &str = "meta_gym";
        const PR: u64 = 75440;
        const URL: &str = "https://api.github.com/repos/tgstation/tgstation";
        const ID: u64 = 3234987;

        let tempdir = tempdir().expect("Failed to create tempdir");

        {
            let job = Job {
                repo: github_types::Repository {
                    url: URL.to_owned(),
                    id: ID,
                },
                base: Branch {
                    sha: BASE.to_owned(),
                    r#ref: BASE_REF.to_owned(),
                },
                head: Branch {
                    sha: HEAD.to_owned(),
                    r#ref: HEAD_REF.to_owned(),
                },
                pull_request: PR,
                files: vec![FileDiff {
                    filename: "_maps/map_files/MetaStation/MetaStation.dmm".to_owned(),
                    status: ChangeType::Modified,
                }],
                check_run: None,
                installation: InstallationId(0),
            };

            let result = do_job(job, None).expect("Failed to finish rendering");

            println!("Result: {:#?}", result);
        }

        tempdir
            .close()
            .expect("Failed to clean up temporary directory");
    }
}
