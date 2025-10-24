# Status Page Generator for EESSI

This repository contains the source code for an EESSI status page generator. The generator scrapes servers for their status and generates a static HTML page with the results.

## Features

- Scrapes server statuses and generates a static HTML status page.
- Configurable via a JSON configuration file.
- Generates both HTML and JSON status reports.
- Automatically populates required resources (images, fonts, CSS, JS, templates, etc.) into the destination directory.
- Supports local editing of resource files, and overwriting them back to the defaults with the `--force` option.
- Evaluates rules for status conditions using [Rhai](https://rhai.rs).
- Supports CVMFS, S3, and AutoDetect as backends for CVMFS servers.

## Installation

- Install [Rust](https://www.rust-lang.org/tools/install)
- `mkdir /tmp/build-dir && cd /tmp/build-dir`
- `git clone https://github.com/terjekv/cvmfs-status-page-rust`
- `cd cvmfs-status-page-rust && cargo build --release`

## Configuration

Create a configuration file (e.g., config.json). See [config.json](config.json) for an example. The only optional key is `backend_type` for servers. It defaults to `AutoDetect` if missing. See the section on server backend types for more information.

Note that `limit_scraping_to_repositories` controls how the scraper determines which repositories to scrape from each server. If set to `true`, only the repositories explicitly listed as `repositories` in the configuration will be scraped (and `ignored_repositories` will have no meaning). If set to `false`, the scraper will also consider repositories detected from the server itself (if applicable), filtered by `ignored_repositores`. The default is `false`.

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
--prometheus-metrics, -p: Enable Prometheus metrics generation.
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

There are four supported status conditions that are evaluated:

- `eessi_status`: The overall status for EESSI.
- `stratum0_servers`: The status for stratum0 servers.
- `stratum1_servers`: The status for stratum1 servers.
- `sync_servers`: The status for sync servers.

Each of these status conditions can have any number of rules associated with them, each with a `status` key that can be set to `OK`, `DEGRADED`, `WARNING`, or `FAILED`. The rules are evaluated in order, and the first matching rule will set the status for the condition in question.

Rules for conditions are evaluated using [Rhai](https://rhai.rs), and are evaluated in order. The first matching rule will set the given status for the case in question. Valid variables for the conditions are:

- `stratum0_servers`: The number of stratum0 servers successfully scraped
- `stratum1_servers`: The number of stratum1 servers successfully scraped
- `sync_servers`: The number of sync servers successfully scraped
- `repos_out_of_sync`: The number of repositories out of sync across all servers scraped

### Example of rules

Imagine these conditions for the overall status, `eessi_status`:

```json
{
    "id": "eessi_status",
    "description": "EESSI status",
    "conditions": [
        {
            "status": "FAILED",
            "when": "stratum1_servers == 0"
        },
        {
            "status": "WARNING",
            "when": "stratum0_servers == 0 && stratum1_servers > 1"
        },
        {
            "status": "WARNING",
            "when": "sync_servers == 0 && stratum1_servers > 1"
        },
        {
            "status": "DEGRADED",
            "when": "stratum0_servers == 1 && stratum1_servers == 1"
        },
        {
            "status": "DEGRADED",
            "when": "repos_out_of_sync > 1"
        },
        {
            "status": "OK",
            "when": "stratum0_servers > 0 && stratum1_servers > 1 && sync_servers > 0"
        }
    ]
}
```

In this example, as the rules are applied in order, the engine will check, in order:

1. If there are no stratum1 servers online, the status is set to `FAILED`.
2. If there are no stratum0 servers online and more than one stratum1 server, the status is set to `WARNING`.
3. If there are no sync servers online and more than one stratum1 server, the status is set to `WARNING`.
4. If the stratum0 server is online and only one stratum1 server was found, the status is set to `DEGRADED`.
5. If more than one repository is out of sync, the status is set to `DEGRADED`.
6. If there is at least one stratum0 server, more than one stratum1 server, and at least one sync server, the status is set to `OK`.

## Prometheus Metrics

Prometheus metrics can be enabled with the `--prometheus-metrics` option. The metrics are exposed as the file `metrics` in the
output directory and are generated with the timestamp being the start of the application.

The status codes used in the metrics are as follows:

- `0`: OK
- `1`: Degraded
- `2`: Warning
- `3`: Failed
- `9`: Maintenance

A typical metrics file might look like this:

```prometheus
# HELP eessi_status EESSI status
# TYPE eessi_status gauge
eessi_status 2 1720525887957
# HELP stratum0_status Stratum0 status
# TYPE stratum0_status gauge
stratum0_status 3 1720525887957
# HELP stratum1_status Stratum1 status
# TYPE stratum1_status gauge
stratum1_status 0 1720525887957
# HELP syncservers_status SyncServers status
# TYPE syncservers_status gauge
syncservers_status 0 1720525887957
# HELP repositories_status Repositories status
# TYPE repositories_status gauge
repositories_status 0 1720525887957
# HELP status_overview Status overview
# TYPE status_overview gauge
status_overview{category="overall"} 0 1761206997670
status_overview{category="stratum0"} 0 1761206997670
status_overview{category="stratum1"} 0 1761206997670
status_overview{category="syncservers"} 0 1761206997670
status_overview{category="repositories"} 0 1761206997670
# HELP repo_catalogue_size Repository catalogue size
# TYPE repo_catalogue_size gauge
repo_catalogue_size{type="stratum0",server="rug-nl-s0.eessi.science",repository="dev.eessi.io"} 9526272 1761206997670
repo_catalogue_size{type="stratum0",server="rug-nl-s0.eessi.science",repository="riscv.eessi.io"} 26624 1761206997670
repo_catalogue_size{type="stratum0",server="rug-nl-s0.eessi.science",repository="software.eessi.io"} 133120 1761206997670
repo_catalogue_size{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="dev.eessi.io"} 9526272 1761206997670
repo_catalogue_size{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="riscv.eessi.io"} 26624 1761206997670
repo_catalogue_size{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 133120 1761206997670
repo_catalogue_size{type="stratum1",server="azure-us-east-s1.eessi.science",repository="dev.eessi.io"} 9526272 1761206997670
repo_catalogue_size{type="stratum1",server="azure-us-east-s1.eessi.science",repository="riscv.eessi.io"} 26624 1761206997670
repo_catalogue_size{type="stratum1",server="azure-us-east-s1.eessi.science",repository="software.eessi.io"} 133120 1761206997670
repo_catalogue_size{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="dev.eessi.io"} 9526272 1761206997670
repo_catalogue_size{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="riscv.eessi.io"} 26624 1761206997670
repo_catalogue_size{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="software.eessi.io"} 133120 1761206997670
repo_catalogue_size{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="dev.eessi.io"} 9526272 1761206997670
repo_catalogue_size{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="riscv.eessi.io"} 26624 1761206997670
repo_catalogue_size{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="software.eessi.io"} 133120 1761206997670
# HELP repo_revision Repository revision
# TYPE repo_revision gauge
repo_revision{type="stratum0",server="rug-nl-s0.eessi.science",repository="dev.eessi.io"} 415 1761206997670
repo_revision{type="stratum0",server="rug-nl-s0.eessi.science",repository="riscv.eessi.io"} 522 1761206997670
repo_revision{type="stratum0",server="rug-nl-s0.eessi.science",repository="software.eessi.io"} 9744 1761206997670
repo_revision{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="dev.eessi.io"} 415 1761206997670
repo_revision{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="riscv.eessi.io"} 522 1761206997670
repo_revision{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 9744 1761206997670
repo_revision{type="stratum1",server="azure-us-east-s1.eessi.science",repository="dev.eessi.io"} 415 1761206997670
repo_revision{type="stratum1",server="azure-us-east-s1.eessi.science",repository="riscv.eessi.io"} 522 1761206997670
repo_revision{type="stratum1",server="azure-us-east-s1.eessi.science",repository="software.eessi.io"} 9744 1761206997670
repo_revision{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="dev.eessi.io"} 415 1761206997670
repo_revision{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="riscv.eessi.io"} 522 1761206997670
repo_revision{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="software.eessi.io"} 9744 1761206997670
repo_revision{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="dev.eessi.io"} 415 1761206997670
repo_revision{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="riscv.eessi.io"} 522 1761206997670
repo_revision{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="software.eessi.io"} 9744 1761206997670
# HELP repo_timestamp Repository timestamp
# TYPE repo_timestamp gauge
repo_timestamp{type="stratum0",server="rug-nl-s0.eessi.science",repository="dev.eessi.io"} 1760706941 1761206997670
repo_timestamp{type="stratum0",server="rug-nl-s0.eessi.science",repository="riscv.eessi.io"} 1750670430 1761206997670
repo_timestamp{type="stratum0",server="rug-nl-s0.eessi.science",repository="software.eessi.io"} 1761150935 1761206997670
repo_timestamp{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="dev.eessi.io"} 1760706941 1761206997670
repo_timestamp{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="riscv.eessi.io"} 1750670430 1761206997670
repo_timestamp{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 1761150935 1761206997670
repo_timestamp{type="stratum1",server="azure-us-east-s1.eessi.science",repository="dev.eessi.io"} 1760706941 1761206997670
repo_timestamp{type="stratum1",server="azure-us-east-s1.eessi.science",repository="riscv.eessi.io"} 1750670430 1761206997670
repo_timestamp{type="stratum1",server="azure-us-east-s1.eessi.science",repository="software.eessi.io"} 1761150935 1761206997670
repo_timestamp{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="dev.eessi.io"} 1760706941 1761206997670
repo_timestamp{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="riscv.eessi.io"} 1750670430 1761206997670
repo_timestamp{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="software.eessi.io"} 1761150935 1761206997670
repo_timestamp{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="dev.eessi.io"} 1760706941 1761206997670
repo_timestamp{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="riscv.eessi.io"} 1750670430 1761206997670
repo_timestamp{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="software.eessi.io"} 1761150935 1761206997670
# HELP repo_ttl Repository TTL
# TYPE repo_ttl gauge
repo_ttl{type="stratum0",server="rug-nl-s0.eessi.science",repository="dev.eessi.io"} 240 1761206997670
repo_ttl{type="stratum0",server="rug-nl-s0.eessi.science",repository="riscv.eessi.io"} 240 1761206997670
repo_ttl{type="stratum0",server="rug-nl-s0.eessi.science",repository="software.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="dev.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="riscv.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="azure-us-east-s1.eessi.science",repository="dev.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="azure-us-east-s1.eessi.science",repository="riscv.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="azure-us-east-s1.eessi.science",repository="software.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="dev.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="riscv.eessi.io"} 240 1761206997670
repo_ttl{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000",repository="software.eessi.io"} 240 1761206997670
repo_ttl{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="dev.eessi.io"} 240 1761206997670
repo_ttl{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="riscv.eessi.io"} 240 1761206997670
repo_ttl{type="syncserver",server="aws-eu-west-s1-sync.eessi.science",repository="software.eessi.io"} 240 1761206997670

```
