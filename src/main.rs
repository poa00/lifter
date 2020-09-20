#[macro_use]
extern crate fstrings;

use anyhow::Result;
use itertools::Itertools;
use log::*;
use rayon::prelude::*;
use scraper::{Html, Selector};
use std::error::Error;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use structopt::StructOpt;

const PATTERN: &str = r###"(?P<binname>[a-zA-Z][a-zA-Z0-9_]+)-(?P<version>(?:[0-9]+\.[0-9]+)(?:\.[0-9]+)*)-(?P<platform>(?:[a-zA-Z0-9_]-?)+)"###;

#[derive(Default, Debug)]
struct Config {
    url_template: String,
    project: String,
    pattern: String,
    version: Option<String>,
    target_platform: Option<String>,
    target_filename: String,

    /// More direct strategy
    /// The HTTP page link that contains the download link
    page_url: String,
    /// The download anchor tag selector. The "href" of the tag will be used.
    /// This will likely match many items, e.g. if there are multiple downloads for different
    /// versions and platforms.
    anchor_tag: String,
    /// This will be matched
    anchor_text: String,
    /// The version tag to check. The "text" of the tag will be used.
    version_tag: Option<String>,
    /// Target filename inside archive. Leave blank if download is not an archive.
    target_filename_to_extract_from_archive: Option<String>,
    /// After download/extraction, rename file to this
    desired_filename: Option<String>,
}

impl Config {
    fn new() -> Config {
        Config {
            url_template: String::from("https://github.com/{project}/releases"),
            pattern: String::from(PATTERN),
            ..Default::default()
        }
    }
}

struct Hit {
    version: String,
    download_url: String,
}

#[derive(structopt::StructOpt)]
#[structopt()]
struct Args {
    #[structopt(short = "p", long = "project", env = "PROJECT", default_value = "blah")]
    project: String,
    /// Silence all output
    #[structopt(short = "q", long = "quiet")]
    quiet: bool,
    /// Verbose mode (-v, -vv, -vvv, etc)
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: usize,
    /// Timestamp (sec, ms, ns, none)
    #[structopt(short = "t", long = "timestamp")]
    ts: Option<stderrlog::Timestamp>,
}

#[paw::main]
fn main(args: Args) -> Result<()> {
    stderrlog::new()
        .module(module_path!())
        .quiet(args.quiet)
        .verbosity(args.verbose)
        .timestamp(args.ts.unwrap_or(stderrlog::Timestamp::Off))
        .init()
        .unwrap();
    trace!("trace message");
    debug!("debug message");
    info!("info message");
    warn!("warn message");
    error!("error message");

    let filename = "binsync.config";
    let conf = tini::Ini::from_file(&filename).unwrap();
    let sections = conf.iter().collect_vec();
    sections.par_iter().for_each(
        |(section, hm)| match run_section(section, &conf, filename) {
            Ok(_) => (),
            Err(e) => {
                error!("{}", e);
            }
        },
    );
    Ok(())
}

fn run_section(section: &str, conf: &tini::Ini, filename: &str) -> Result<()> {
    // Happy helper for getting a value in this section
    let get = |s: &str| conf.get::<String>(&section, s);
    let mut cf = Config::new();

    // First get the project - required
    match get("page_url") {
        Some(p) => cf.page_url = p,
        None => {
            return {
                warn!("Section {} is missing required field \"page_url\"", section);
                Ok(())
            };
        }
    };
    debug!("Processing: {}", &cf.page_url);

    // Now the remaining values
    cf.anchor_tag = get("anchor_tag").unwrap();
    cf.anchor_text = get("anchor_text").unwrap();
    cf.version_tag = get("version_tag");
    cf.target_filename_to_extract_from_archive =
        if let Some(name) = get("target_filename_to_extract_from_archive") {
            Some(name)
        } else {
            Some(section.to_owned())
        };
    cf.version = get("version");
    cf.desired_filename = get("desired_filename");

    if !std::path::Path::new(&cf.target_filename).exists() {
        if let Some(new_version) = process(&mut cf)? {
            // New version, must update the version number in the
            // config file.
            info!(
                "Downloaded new version of {}: {}",
                &cf.target_filename, &new_version
            );
            // TODO: actually need a mutex around the following 3 lines.
            let conf_write = tini::Ini::from_file(&filename).unwrap();

            conf_write
                .section(section)
                .item("version", &new_version)
                .to_file(&filename)
                .unwrap();
            debug!("Updated config file.");
        }
    } else {
        info!("Target {} exists, skipping.", &cf.target_filename);
    }
    Ok(())
}

fn target_file_already_exists(conf: &Config) -> bool {
    let filename_to_check = if let Some(fname) = conf.desired_filename.as_ref() {
        fname
    } else if let Some(fname) = conf.target_filename_to_extract_from_archive.as_ref() {
        fname
    } else {
        panic!("This should be impossible")
    };

    std::path::Path::new(&filename_to_check).exists()
}

fn process(conf: &mut Config) -> Result<Option<String>> {
    let url = &conf.page_url;

    let parse_result = parse_html_page(&conf, url)?;
    let hit = match parse_result {
        Some(hit) => hit,
        None => return Ok(None),
    };

    let existing_version = conf.version.as_ref().unwrap();
    if target_file_already_exists(&conf) && &hit.version <= existing_version {
        debug!("Found version is not newer: {}; Skipping.", &hit.version);
        return Ok(None);
    }
    info!("Downloading version {}", &hit.version);

    let download_url = &hit.download_url;
    let ext = {
        if vec![".tar.gz", ".tgz"]
            .iter()
            .any(|ext| download_url.ends_with(ext))
        {
            ".tar.gz"
        } else if download_url.ends_with(".tar.xz") {
            ".tar.xz"
        } else if download_url.ends_with(".zip") {
            ".zip"
        } else if download_url.ends_with(".exe") {
            ".exe"
        } else {
            warn!("Failed to match known file extensions. Skipping.");
            return Ok(None);
        }
    };

    let mut resp = reqwest::blocking::get(download_url)?;
    let mut buf: Vec<u8> = Vec::new();
    resp.copy_to(&mut buf)?;

    // if let Some(target_filename) = match conf.target_filename_to_extract_from_archive {
    //
    // };
    // let dlfilename = if let Some(filename) = &conf.target_filename_to_extract_from_archive {
    //     filename
    // } else if let Some(filename) = &conf.desired_filename {
    //     filename
    // } else {
    //     return Err(anyhow::Error::msg(
    //         "Either \"desired_filename\" or \"target_filename\" must be given",
    //     ));
    // }
    //     .clone() + ext;

    if ext == ".tar.xz" {
        // TODO: this should return the file that got created; and then we can
        //  decide if to rename that file.
        extract_target_from_tarxz(&mut buf, &conf);
        if let Some(desired_filename) = &conf.desired_filename {
            let extracted_filename = conf
                .target_filename_to_extract_from_archive
                .as_ref()
                .unwrap();
            if desired_filename != extracted_filename {
                debug!(
                    "Extract filename is different to desired, renaming {} \
                            to {}",
                    extracted_filename, desired_filename
                );
                std::fs::rename(extracted_filename, desired_filename)?;
            }
        }
    } else if ext == ".zip" {
        extract_target_from_zipfile(&mut buf, &conf);
        if let Some(desired_filename) = &conf.desired_filename {
            let extracted_filename = conf
                .target_filename_to_extract_from_archive
                .as_ref()
                .unwrap();
            if desired_filename != extracted_filename {
                debug!(
                    "Extract filename is different to desired, renaming {} \
                            to {}",
                    extracted_filename, desired_filename
                );
                std::fs::rename(extracted_filename, desired_filename)?;
            }
        }
    } else if ext == ".tar.gz" {
        extract_target_from_tarfile(&mut buf, &conf);
        if let Some(desired_filename) = &conf.desired_filename {
            let extracted_filename = conf
                .target_filename_to_extract_from_archive
                .as_ref()
                .unwrap();
            if desired_filename != extracted_filename {
                debug!(
                    "Extract filename is different to desired, renaming {} \
                            to {}",
                    extracted_filename, desired_filename
                );
                std::fs::rename(extracted_filename, desired_filename)?;
            }
        }
    } else if ext == ".exe" {
        // Windows executables are not compressed, so we only need to
        // handle renames, if the option is given.
        let desired_filename = conf.desired_filename.as_ref().unwrap();
        let mut output = std::fs::File::create(&desired_filename)?;
        info!("Saving {} to {}", &download_url, desired_filename);
        output.write_all(&buf)?;
    };

    Ok(Some(hit.version))
}

fn parse_html_page(conf: &Config, url: &str) -> Result<Option<Hit>> {
    debug!("Fetching page at {}", &url);
    let resp = reqwest::blocking::get(url)?;
    let body = resp.text()?;

    debug!("Setting up parsers");
    let fragment = Html::parse_document(&body);
    let stories = match Selector::parse(&conf.anchor_tag) {
        Ok(s) => s,
        Err(e) => {
            warn!("Parser error at {}: {:?}", url, e);
            return Ok(None);
        }
    };
    let versions = Selector::parse(conf.version_tag.as_ref().unwrap()).unwrap();
    let re_pat = regex::Regex::new(&conf.anchor_text)?;

    debug!("Looking for matches...");
    for story in fragment.select(&stories) {
        if let Some(href) = &story.value().attr("href") {
            // This is the download target in the matched link
            let download_url = format!("https://github.com{}", &href);
            debug!("download_url: {}", &download_url);

            let caps = match re_pat.captures_iter(&href).next() {
                Some(c) => {
                    debug!("Found a match for anchor_text");
                    c
                }
                None => continue,
            };

            return if let Some(raw_version) = fragment.select(&versions).next() {
                let version = raw_version.text().join("");
                info!("Found a match on versions tag: {}", version);
                Ok(Some(Hit {
                    version,
                    download_url,
                }))
            } else {
                warn!(
                    "Download link {} was found but failed to match version \
                       tag \"{}\"",
                    &download_url,
                    conf.version_tag.as_ref().unwrap()
                );
                Ok(None)
            };
        }
    }
    warn!("Matched nothing at url {}", url);
    Ok(None)
}

fn extract_target_from_zipfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = zip::ZipArchive::new(&mut cbuf).unwrap();

    let target_filename = conf
        .target_filename_to_extract_from_archive
        .as_ref()
        .expect(
            "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
        );

    for fname in archive
        .file_names()
        // What's dumb is that the borrow below `by_name` is a mutable
        // borrow, which means that an immutable borrow for
        // `archive.file_names` won't be allowed. To work around this,
        // for now just collect all the filenames into a long list.
        // Since we're looking for a specific name, it would be more
        // efficient to first find the name, leave the loop, and in the
        // next section do the extraction.
        .map(String::from)
        .collect::<Vec<String>>()
    {
        let mut file = archive.by_name(&fname).unwrap();
        let path = std::path::Path::new(&fname);
        debug!(
            "zip, got filename: {}",
            &path.file_name().unwrap().to_str().unwrap()
        );
        if let Some(p) = &path.file_name() {
            if &p.to_string_lossy() == target_filename {
                debug!("zip, Got a match: {}", &fname);
                let mut rawfile = std::fs::File::create(&target_filename).unwrap();
                let mut buf = Vec::new();
                file.read_to_end(&mut buf).unwrap();
                rawfile.write_all(&buf).unwrap();
                return;
            }
        }
    }

    warn!(
        "Failed to find file inside archive: \"{}\"",
        &target_filename
    );
}

fn extract_target_from_tarfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut gzip_archive = flate2::read::GzDecoder::new(&mut cbuf);
    let mut archive = tar::Archive::new(gzip_archive);

    let target_filename = conf
        .target_filename_to_extract_from_archive
        .as_ref()
        .expect(
            "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
        );

    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();
        trace!("This is what I found in the tar.xz: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        debug!(
            "tar.gz, got filename: {}",
            &raw_path.file_name().unwrap().to_str().unwrap()
        );

        if let Some(p) = &raw_path.file_name() {
            // println!("path: {:?}", &p);
            if let Some(pm) = p.to_str() {
                // println!("stem: {:?}", &pm);
                if pm == target_filename {
                    debug!("tar.gz, Got a match: {}", &pm);
                    // println!("We found a match: {}", &pm);
                    // println!("Raw headers: {:?}", &file.header());
                    file.unpack(&target_filename).unwrap();
                    return;
                }
            }
        }
    }

    warn!(
        "Failed to find file \"{}\" inside archive",
        &target_filename
    );
}

fn extract_target_from_tarxz(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut buf: Vec<u8> = Vec::new();
    let mut bw = std::io::Cursor::new(&mut buf);

    // lzma_rs::xz_decompress(&mut cbuf, &mut bw).expect("Problem xz_decompress");

    let mut decompressor = xz2::read::XzDecoder::new(cbuf);

    // let mut xzf = lzma::LzmaReader::new_decompressor(cbuf).expect("Problem decompressing");

    // let decode_options = lzma_rs::decompress::Options {
    //     unpacked_size: lzma_rs::decompress::UnpackedSize::ReadFromHeader,
    // };
    // lzma_rs::lzma_decompress_with_options(&mut cbuf, &mut bw, &decode_options)
    //     .expect("Problem lzma_decompress_with_options");

    // let mut c = std::io::Cursor::new(&mut bw);
    let mut archive = tar::Archive::new(&mut decompressor);

    let target_filename = conf
        .target_filename_to_extract_from_archive
        .as_ref()
        .expect(
            "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
        );

    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();
        trace!("This is what I found in the tar.xz: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        debug!(
            "tar.gz, got filename: {}",
            &raw_path.file_name().unwrap().to_str().unwrap()
        );

        if let Some(p) = &raw_path.file_name() {
            // println!("path: {:?}", &p);
            if let Some(pm) = p.to_str() {
                // println!("stem: {:?}", &pm);
                if pm == target_filename {
                    debug!("tar.gz, Got a match: {}", &pm);
                    // println!("We found a match: {}", &pm);
                    // println!("Raw headers: {:?}", &file.header());
                    file.unpack(&target_filename).unwrap();
                    return;
                }
            }
        }
    }

    warn!(
        "Failed to find file \"{}\" inside archive",
        &target_filename
    );
}
