# Status Page Generator for EESSI

This repository contains the source code for an EESSI status page generator. The generator scrapes servers for their status and generates a static HTML page with the results.

## Features

- Scrapes server statuses and generates a static HTML status page.
- Configurable via a JSON configuration file.
- Generates both HTML and JSON status reports.
- Automatically populates required resources (images, fonts, CSS, JS, templates, etc.) into the destination directory.
- Supports local editing of resource files, and overwriting them back to the defaults with the `--force` option.
- Evaluates rules for status conditions using [https://rhai.rs](Rhai).
- Supports CVMFS, S3, and AutoDetect as backends for CVMFS servers.

## Installation

- Install [Rust](https://www.rust-lang.org/tools/install)
- `mkdir /tmp/build-dir && cd /tmp/build-dir`
- `git clone https://github.com/terjekv/cvmfs-server-scraper-rust`
- `git clone https://github.com/terjekv/cvmfs-status-page-rust`
- `cd cvmfs-status-page-rust && cargo build --release`

## Configuration

Create a configuration file (e.g., config.json). See [config.json](config.json) for an example. The only optional key is `backend_type` for servers. It defaults to `AutoDetect` if missing. See the section on server backend types for more information.

## Usage

Run the binary with the desired options:

```sh
./cvmfs-status-page-rust --destination /path/to/output --configuration /path/to/config.json
```

### Command Line Options

```sh
--destination, -d: Destination directory for the generated status page. Default is the current directory.
--configuration, -c: Path to the configuration file. Default is config.json.
--show-config, -s: Show the configuration and exit.
--force-resource-creation, -f: Force overwrite of existing files.
--output-file, -o: Filename for the generated status page. Default is index.html.
--json-output-file, -j: Filename for the generated JSON status. Default is status.json.
```

### Example

```sh
./cvmfs-status-page-rust -d ./output -c ./config.json -o status.html -j status.json
```

## Logging

Set the RUST_LOG environment variable to your desired log level for logging. For example:

```sh
RUST_LOG=info ./cvmfs-status-page-rust -c config.json
```

## Resources

Resources such as images, fonts, CSS, JS, and templates will be populated into the destination directory from the binary if missing. These resources can be edited locally as their existience will prevent recreation. To reinstall the shipped versions, issue the --force option.

## Server Backend Types

- `CVMFS`: Requires `cvmfs/info/v1/repositories.json` to be present on the server. Scrape fails if it is missing.
- `S3`: Does not even attempt to fetch `cvmfs/info/v1/repositories.json`. Note that if any server has S3 as a backend the configuration entry repositories *must* be present and contain the list of repositories to be scraped (there is no other way to determine the list of repositories for S3 servers). Due to the async scraping of all servers, there is currently no support for falling back on repositories detected from other server types (including the Stratum0).
- `AutoDetect`: Attempts to fetch `cvmfs/info/v1/repositories.json` but does not fail if it is missing. If the scraper fails to fetch the file, the backend will be assumed to be S3.

For servers that are set to or detected as CVMFS, the scraper will scrape the union of the detected and configurations explicitly stated repositories.

## Condition Evaluation for Status

Rules for conditions are evaluated using Rhai, and are evaluated in order. Valid variables for the conditions are:

- `stratum0_servers`: The number of stratum0 servers successfully scraped
- `stratum1_servers`: The number of stratum1 servers successfully scraped
- `sync_servers`: The number of sync servers successfully scraped
- `repos_out_of_sync`: The number of repositories out of sync across all servers scraped
