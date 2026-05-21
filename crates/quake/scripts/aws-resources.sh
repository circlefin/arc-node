#!/usr/bin/env bash
#
# List and remove AWS resources created by `quake remote create` when the
# Terraform state is no longer available to drive `quake clean`.
#
# Identification: anchored VPC traversal. The VPC is found by its Name tag
# (equal to the project name); child resources are enumerated via `--filters
# Name=vpc-id,Values=<vpc>`. The key pair and CloudWatch alarm live outside the
# VPC and are looked up by deterministic name.
#
# Project name: arc-<sanitized-testnet>-testnet-<sanitized-github-user>,
# mirroring Terraform's `lower(replace(x, "/[/.]/", "-"))` sanitization.
#
# Requirements:
#   - AWS CLI v2 (uses JMESPath filter syntax such as Tags[?Key==`Name`]), with
#     AWS credentials configured.

# `set -u` is intentionally omitted: Bash 3.2 (macOS default) reports
# `"${arr[@]}"` as an unbound variable for empty arrays, even when the array
# was declared with `arr=()`. Keeping nounset would require wrapping every
# array expansion in `${arr[@]+"${arr[@]}"}`, which is worse than losing it.
set -eo pipefail

# --- Constants ---------------------------------------------------------------

# Marker tag key applied to every Quake EC2 instance via var.tags.
MARKER_TAG_KEY="arc-quake-testnet"

# Project name prefix and infix used for all Quake projects.
PROJECT_PREFIX="arc-"
PROJECT_INFIX="-testnet-"

# Instance-state filter values for live (non-terminated) instances. Used in
# describe-instances filters as Name=instance-state-name,Values=$INSTANCE_LIVE_STATES.
INSTANCE_LIVE_STATES="pending,running,stopping,stopped,shutting-down"

# --- Globals (populated by parse_args) ---------------------------------------

SUBCOMMAND=""
REGION=""
USER_OVERRIDE=""
USER_OVERRIDE_PASSED=false
SKIP_PROMPT=false
VERBOSE=false
TESTNET_ARG=""

# State shared between plan and apply.
declare -a FAILURES=()

# --- Output helpers ----------------------------------------------------------

# Data on stdout; progress, warnings, and errors on stderr.

out() { printf '%s\n' "$*"; }
log() { printf '%s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
err() { printf 'error: %s\n' "$*" >&2; }

die() {
  err "$1"
  exit "${2:-1}"
}

die_usage() {
  err "$1"
  usage >&2
  exit 2
}

# TTY-aware color codes. Only active when stdout is a terminal.
if [[ -t 1 ]]; then
  C_BOLD=$'\033[1m'
  C_DIM=$'\033[2m'
  C_RESET=$'\033[0m'
else
  C_BOLD=""
  C_DIM=""
  C_RESET=""
fi

# --- Usage -------------------------------------------------------------------

usage() {
  local prog
  prog="$(basename -- "$0")"
  # Unquoted heredoc so ${prog} expands; env-var references are escaped as \$.
  cat <<EOF
Usage:
  ${prog} list [--region REGION] [--user NAME]
  ${prog} list TESTNET [--region REGION] [--user NAME]
  ${prog} remove TESTNET [--yes] [--region REGION] [--user NAME] [--verbose]

Commands:
  list                Summarize Quake projects for the current user.
  list TESTNET        Show the resource plan for a single project without deleting.
  remove TESTNET      Show the plan, then prompt. With --yes, skip the prompt and delete.

Arguments:
  TESTNET             Testnet basename (e.g., localdev) or full project name
                      (e.g., arc-localdev-testnet-alice). Auto-detected via
                      the "-testnet-" infix.

Options:
  --region REGION     AWS region. Default: \$AWS_REGION, else us-east-1.
  --user NAME         GitHub username. Default: \$GITHUB_USER (no \$USER fallback).
                      Required to operate on a project owned by another user.
  --yes, -y           (remove only) Skip the interactive confirmation prompt.
  --verbose           One line per resource deleted (default: one line per phase).
  -h, --help          Show this help.
EOF
}

# --- Utilities ---------------------------------------------------------------

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    die "required command not found: $1"
  fi
}

# True when an AWS CLI text-output value indicates "no such resource". The CLI
# emits an empty string when a query returns nothing, and the literal "None"
# when a queried field is null on an existing resource.
is_absent_or_none() {
  [[ -z "${1:-}" || "$1" == "None" ]]
}

# Mirror Terraform's lower(replace(x, "/[/.]/", "-")).
sanitize() {
  local input="${1:-}"
  [[ -z "$input" ]] && return
  printf '%s' "$input" | tr '[:upper:]' '[:lower:]' | tr '/.' '--'
}

# Resolve the repository root. Prefer git; fall back to path math from BASH_SOURCE.
resolve_repo_root() {
  local script_dir root
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  if root="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null)"; then
    printf '%s' "$root"
    return
  fi
  # Fallback: this script lives at crates/quake/scripts/, so repo root is 3 up.
  (cd "$script_dir/../../.." && pwd)
}

# Source <repo_root>/.env only if GITHUB_USER is unset.
load_dotenv_if_needed() {
  if [[ -n "${GITHUB_USER:-}" ]]; then
    return
  fi
  local dotenv="$1/.env"
  if [[ -f "$dotenv" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$dotenv"
    set +a
  fi
}

# Resolved effective user for project-name construction (already sanitized).
# --user takes precedence over $GITHUB_USER; missing both is a usage error.
resolve_effective_user() {
  if [[ -n "$USER_OVERRIDE" ]]; then
    sanitize "$USER_OVERRIDE"
    return
  fi
  if [[ -z "${GITHUB_USER:-}" ]]; then
    die_usage "--user not provided and GITHUB_USER is not set"
  fi
  sanitize "$GITHUB_USER"
}

# AWS CLI wrapper that pins the region.
aws_cli() {
  aws --region "$REGION" "$@"
}

# Run an AWS describe/query and emit its `--output text` result split by whitespace
# (tabs and newlines) into one line per value. Silent on missing resources.
# Intended only for single-column queries.
collect() {
  local raw
  raw="$(aws_cli "$@" --output text 2>/dev/null || true)"
  if is_absent_or_none "$raw"; then
    return
  fi
  # AWS text output separates list elements with tabs; normalize.
  tr '[:space:]' '\n' <<<"$raw" | sed '/^$/d'
}

# Run an AWS describe/query expected to return multi-column text output where
# the first column is the resource ID and the second is a human-readable name
# (or "None" when absent). Emits one tab-separated record per line; silent on
# missing resources.
collect_pairs() {
  local raw
  raw="$(aws_cli "$@" --output text 2>/dev/null || true)"
  if [[ -z "$raw" ]]; then
    return
  fi
  local line first
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    first="${line%%$'\t'*}"
    is_absent_or_none "$first" && continue
    printf '%s\n' "$line"
  done <<< "$raw"
}

# Bash 3 replacement for `mapfile -t NAME < <(cmd)`. Reads newline-delimited
# input on stdin and assigns each line to the named array, escaping values
# through `%q` so whitespace or shell metacharacters survive the eval.
read_lines() {
  local __name="$1"
  eval "$__name=()"
  local __line __escaped
  while IFS= read -r __line; do
    printf -v __escaped '%q' "$__line"
    eval "$__name+=($__escaped)"
  done
}

# --- Project name handling ---------------------------------------------------

# True if the argument looks like a fully-qualified project name.
# Requires both the arc- prefix and the -testnet- infix to reduce false positives
# on testnet basenames that happen to contain "-testnet-" as a substring.
# Caller MUST pass a sanitized (lowercase) string — the constants are lowercase
# and bash glob matches are case-sensitive.
is_full_project_name() {
  local arg="$1"
  [[ "$arg" == ${PROJECT_PREFIX}* && "$arg" == *${PROJECT_INFIX}* ]]
}

# Given a testnet argument and the effective (sanitized) user, return the project name.
# The argument is sanitized before classification so an uppercased full project
# name (e.g. ARC-LOCALDEV-TESTNET-ALICE) is not mistaken for a testnet basename
# and double-wrapped with the arc- prefix and -testnet- infix.
project_name_from_arg() {
  local arg="$1" effective_user="$2"
  local normalized
  normalized="$(sanitize "$arg")"
  if is_full_project_name "$normalized"; then
    printf '%s' "$normalized"
  else
    local testnet
    testnet="$(sanitize "$(basename -- "$arg")")"
    printf '%s%s%s%s' "$PROJECT_PREFIX" "$testnet" "$PROJECT_INFIX" "$effective_user"
  fi
}

# Extract the user segment (text after the last "-testnet-") from a project name.
user_from_project() {
  local project="$1"
  printf '%s' "${project##*${PROJECT_INFIX}}"
}

# Extract the testnet segment (text between the "arc-" prefix and the last
# "-testnet-" infix) from a project name.
testnet_from_project() {
  local project="$1"
  local without_prefix="${project#${PROJECT_PREFIX}}"
  # %${PROJECT_INFIX}* is shortest-match from end, i.e. strips from the LAST
  # occurrence of "-testnet-" onwards. This keeps testnet names that happen to
  # contain "-testnet-" as a substring intact.
  printf '%s' "${without_prefix%${PROJECT_INFIX}*}"
}

# Cross-user guardrail: refuse to operate on someone else's project unless --user
# was explicitly passed. Only relevant when the argument was a full project name;
# for testnet basenames, the constructed project name always uses the effective user.
check_cross_user_guardrail() {
  local project="$1"
  if [[ "$USER_OVERRIDE_PASSED" == "true" ]]; then
    return
  fi
  local project_user gh_sanitized
  project_user="$(user_from_project "$project")"
  gh_sanitized="$(sanitize "${GITHUB_USER:-}")"
  if [[ -n "$gh_sanitized" && "$project_user" != "$gh_sanitized" ]]; then
    die_usage "project '$project' belongs to user '$project_user' but --user was not passed (current GITHUB_USER resolves to '$gh_sanitized'). Pass --user $project_user to confirm."
  fi
}

# --- Discovery: per-project -------------------------------------------------
#
# Side effects: this function OVERWRITES module-level globals listed below.
# It is the only writer of these arrays; callers must not assume the values
# survive across calls. cmd_list_summary deliberately re-invokes per project
# in a loop, relying on this clean-slate behavior.
#
# Populates parallel *_IDS and *_NAMES arrays for each resource class, indexed
# together (NAMES[i] is the Name tag / GroupName of IDS[i], or "" if absent).
# Key pairs and CloudWatch alarms have no separate name field, so only KEY_NAMES
# and ALARM_NAMES are populated for those.
#
#   VPC_IDS/VPC_NAMES              VPCs with Name tag = <project>
#   INSTANCE_IDS/INSTANCE_NAMES    instances tagged project=<project> OR in the VPCs
#   ENI_IDS/ENI_NAMES              ENIs tagged project=<project> OR in the VPCs
#   VOLUME_IDS/VOLUME_NAMES        EBS volumes tagged project=<project>
#   SUBNET_IDS/SUBNET_NAMES        subnets inside the VPCs
#   RTB_IDS/RTB_NAMES              non-main route tables inside the VPCs
#   SG_IDS/SG_NAMES                non-default security groups inside the VPCs (name = GroupName)
#   IGW_IDS/IGW_NAMES              internet gateways attached to the VPCs
#   KEY_NAMES                      key pairs named <project>-key (ID == name)
#   ALARM_NAMES                    CloudWatch alarms <project>-cc-auto-recovery (ID == name)
#
# EBS volumes: Terraform sets `volume_tags` on aws_instance so root volumes
# inherit `project=<project>`. With delete_on_termination=true (AMI default),
# in-use root volumes vanish on instance termination — discovery here only
# surfaces detached/orphaned volumes, plus root volumes whose instance is
# still running at discovery time. The delete phase re-checks status.
# Volumes created before this tagging change have no `project` tag and are
# invisible to this script; they will still be cleaned up by their attached
# instance's termination via delete_on_termination.

# Scan a describe query into parallel IDS/NAMES arrays, deduping via a seen-map.
# Called once per data source; union scans (instances/ENIs) call it twice with
# the same target arrays and seen-map.
#
#   _scan_into IDS_ARR NAMES_ARR SEEN_VAR  aws-cli-args...
#
# Bash 3 port: namerefs and associative arrays are unavailable, so array
# identities are passed by name and resolved via `eval`, and the seen-map is
# a colon-delimited string probed with a `case` pattern match.
_scan_into() {
  local ids_name="$1" names_name="$2" seen_name="$3"
  shift 3
  local id name id_esc name_esc seen_val
  while IFS=$'\t' read -r id name _; do
    [[ -z "$id" ]] && continue
    eval "seen_val=\"\${$seen_name}\""
    case ":${seen_val}:" in
      *":$id:"*) continue ;;
    esac
    eval "$seen_name=\"\${$seen_name}:\$id\""
    [[ "$name" == "None" ]] && name=""
    printf -v id_esc '%q' "$id"
    printf -v name_esc '%q' "$name"
    eval "$ids_name+=($id_esc)"
    eval "$names_name+=($name_esc)"
  done < <(collect_pairs "$@")
}

discover_project() {
  local project="$1" vpc

  VPC_IDS=();      VPC_NAMES=();      local _s_vpc=""
  INSTANCE_IDS=(); INSTANCE_NAMES=(); local _s_inst=""
  ENI_IDS=();      ENI_NAMES=();      local _s_eni=""
  VOLUME_IDS=();   VOLUME_NAMES=();   local _s_vol=""
  SUBNET_IDS=();   SUBNET_NAMES=();   local _s_subnet=""
  RTB_IDS=();      RTB_NAMES=();      local _s_rtb=""
  SG_IDS=();       SG_NAMES=();       local _s_sg=""
  IGW_IDS=();      IGW_NAMES=();      local _s_igw=""

  local inst_state="Name=instance-state-name,Values=${INSTANCE_LIVE_STATES}"
  local inst_query='Reservations[].Instances[].[InstanceId, Tags[?Key==`Name`].Value|[0]]'
  local eni_query='NetworkInterfaces[].[NetworkInterfaceId, Tags[?Key==`Name`].Value|[0]]'

  # VPCs anchor the traversal.
  _scan_into VPC_IDS VPC_NAMES _s_vpc \
    ec2 describe-vpcs \
    --filters "Name=tag:Name,Values=${project}" \
    --query 'Vpcs[].[VpcId, Tags[?Key==`Name`].Value|[0]]'

  # Instances and ENIs: tag-based first; VPC-scoped later in the per-VPC loop.
  _scan_into INSTANCE_IDS INSTANCE_NAMES _s_inst \
    ec2 describe-instances \
    --filters "Name=tag:project,Values=${project}" "$inst_state" \
    --query "$inst_query"
  _scan_into ENI_IDS ENI_NAMES _s_eni \
    ec2 describe-network-interfaces \
    --filters "Name=tag:project,Values=${project}" \
    --query "$eni_query"

  # EBS volumes: tag-based only. Volumes have no VPC association, so there is
  # no per-VPC fallback. Pulled regardless of state — `available` are the true
  # orphans we will delete; `in-use` ones may attach to instances we are about
  # to terminate (delete_on_termination handles the cleanup) and are surfaced
  # here for visibility only. The delete phase re-checks status to decide.
  _scan_into VOLUME_IDS VOLUME_NAMES _s_vol \
    ec2 describe-volumes \
    --filters "Name=tag:project,Values=${project}" \
    --query 'Volumes[].[VolumeId, Tags[?Key==`Name`].Value|[0]]'

  for vpc in "${VPC_IDS[@]}"; do
    _scan_into INSTANCE_IDS INSTANCE_NAMES _s_inst \
      ec2 describe-instances \
      --filters "Name=vpc-id,Values=${vpc}" "$inst_state" \
      --query "$inst_query"
    _scan_into ENI_IDS ENI_NAMES _s_eni \
      ec2 describe-network-interfaces \
      --filters "Name=vpc-id,Values=${vpc}" \
      --query "$eni_query"
    _scan_into SUBNET_IDS SUBNET_NAMES _s_subnet \
      ec2 describe-subnets \
      --filters "Name=vpc-id,Values=${vpc}" \
      --query 'Subnets[].[SubnetId, Tags[?Key==`Name`].Value|[0]]'
    _scan_into RTB_IDS RTB_NAMES _s_rtb \
      ec2 describe-route-tables \
      --filters "Name=vpc-id,Values=${vpc}" \
      --query 'RouteTables[?Associations[?Main!=`true`] || length(Associations)==`0`].[RouteTableId, Tags[?Key==`Name`].Value|[0]]'
    _scan_into SG_IDS SG_NAMES _s_sg \
      ec2 describe-security-groups \
      --filters "Name=vpc-id,Values=${vpc}" \
      --query 'SecurityGroups[?GroupName!=`default`].[GroupId, GroupName]'
    _scan_into IGW_IDS IGW_NAMES _s_igw \
      ec2 describe-internet-gateways \
      --filters "Name=attachment.vpc-id,Values=${vpc}" \
      --query 'InternetGateways[].[InternetGatewayId, Tags[?Key==`Name`].Value|[0]]'
  done

  # Key pairs and CloudWatch alarms: ID is the only identifier.
  read_lines KEY_NAMES < <(
    collect ec2 describe-key-pairs \
      --filters "Name=key-name,Values=${project}-key" \
      --query 'KeyPairs[].KeyName'
  )
  read_lines ALARM_NAMES < <(
    collect cloudwatch describe-alarms \
      --alarm-names "${project}-cc-auto-recovery" \
      --query 'MetricAlarms[].AlarmName'
  )
}

# --- Discovery: cross-project (for `list` summary) ---------------------------

# Return one project name per line for projects discovered in the region.
# Union of three sources, in order of confidence:
#   1. describe-vpcs filtered by tag-key=$MARKER_TAG_KEY; the VPC's `project`
#      tag value is the project name. This is the authoritative source for
#      VPCs created with the current Terraform.
#   2. describe-vpcs by Name tag matching `arc-*-testnet-*`. Back-compat for
#      VPCs created before the marker tag was added; may include unrelated
#      VPCs that happen to share the Name pattern.
#   3. describe-instances carrying the marker tag key; their `project` tag
#      values. Catches projects whose VPC was already deleted but whose
#      instances are still terminating.
discover_all_projects() {
  local -a vpc_marker_projects=() vpc_names=() instance_projects=()

  read_lines vpc_marker_projects < <(
    collect ec2 describe-vpcs \
      --filters "Name=tag-key,Values=${MARKER_TAG_KEY}" \
      --query 'Vpcs[].Tags[?Key==`project`].Value[]'
  )
  read_lines vpc_names < <(
    collect ec2 describe-vpcs \
      --filters "Name=tag-key,Values=Name" \
      --query 'Vpcs[].Tags[?Key==`Name`].Value[]'
  )
  read_lines instance_projects < <(
    collect ec2 describe-instances \
      --filters "Name=tag-key,Values=${MARKER_TAG_KEY}" \
                "Name=instance-state-name,Values=${INSTANCE_LIVE_STATES}" \
      --query 'Reservations[].Instances[].Tags[?Key==`project`].Value[]'
  )

  # Filter to values shaped like a project name. Returns 0 even when no input
  # matches, so the function is safe to call inside `set -e` contexts.
  _emit_projects() {
    local item
    for item in "$@"; do
      [[ -z "$item" ]] && continue
      if is_full_project_name "$item"; then
        printf '%s\n' "$item"
      fi
    done
    return 0
  }
  {
    _emit_projects "${vpc_marker_projects[@]}"
    _emit_projects "${vpc_names[@]}"
    _emit_projects "${instance_projects[@]}"
  } | sort -u
}

# --- Rendering ---------------------------------------------------------------

# Print only the per-class resource rows (no header, no total). Used both by the
# full plan and by the no-arg `list` summary when expanding leftover resources.
# EC2 instances are always rendered (even with count 0) so the user sees at a
# glance whether anything is still running. Other classes are hidden when empty
# to keep the output tight. Each class header is followed by one resource per
# line, indented two spaces further than the header.
show_plan_classes() {
  local indent="${1:-}"
  local item_indent="${indent}  "

  # Paired row (ID + name). When `always` is "true", render the row even with
  # count zero, showing an indented "(none)". Array identities are passed by
  # name and resolved via `eval` for Bash 3 compatibility.
  _row_pair() {
    local label="$1" ids_name="$2" names_name="$3" always="${4:-false}"
    local count
    eval "count=\${#$ids_name[@]}"
    if ((count == 0)); then
      if [[ "$always" == "true" ]]; then
        printf '%s%s (%d):\n' "$indent" "$label" "$count"
        printf '%s(none)\n' "$item_indent"
      fi
      return
    fi
    printf '%s%s (%d):\n' "$indent" "$label" "$count"
    local i id name
    for ((i=0; i<count; i++)); do
      eval "id=\"\${$ids_name[$i]}\""
      eval "name=\"\${$names_name[$i]:-}\""
      if [[ -n "$name" ]]; then
        printf '%s%s (%s)\n' "$item_indent" "$id" "$name"
      else
        printf '%s%s\n' "$item_indent" "$id"
      fi
    done
  }

  # Simple row (ID is the only identifier). Hidden when empty.
  _row_simple() {
    local label="$1"; shift
    local -a ids=("$@")
    ((${#ids[@]} == 0)) && return
    printf '%s%s (%d):\n' "$indent" "$label" "${#ids[@]}"
    local n
    for n in "${ids[@]}"; do
      printf '%s%s\n' "$item_indent" "$n"
    done
  }

  _row_pair   "EC2 instances"     INSTANCE_IDS INSTANCE_NAMES true
  _row_pair   "VPCs"              VPC_IDS      VPC_NAMES
  _row_pair   "EBS volumes"       VOLUME_IDS   VOLUME_NAMES
  _row_simple "CloudWatch alarms" "${ALARM_NAMES[@]}"
  _row_pair   "Secondary ENIs"    ENI_IDS      ENI_NAMES
  _row_pair   "Security groups"   SG_IDS       SG_NAMES
  _row_pair   "Route tables"      RTB_IDS      RTB_NAMES
  _row_pair   "Subnets"           SUBNET_IDS   SUBNET_NAMES
  _row_pair   "Internet gateways" IGW_IDS      IGW_NAMES
  _row_simple "Key pairs"         "${KEY_NAMES[@]}"
}

# Print the full resource plan for a discovered project (header + classes + total).
# Sets the global PLAN_TOTAL to the sum of discovered resources.
show_plan() {
  local project="$1"
  PLAN_TOTAL=$((${#ALARM_NAMES[@]} + ${#INSTANCE_IDS[@]} + ${#ENI_IDS[@]} + ${#SG_IDS[@]} \
              + ${#RTB_IDS[@]} + ${#SUBNET_IDS[@]} + ${#IGW_IDS[@]} + ${#KEY_NAMES[@]} \
              + ${#VPC_IDS[@]} + ${#VOLUME_IDS[@]}))

  printf '%sProject:%s %s\n' "$C_BOLD" "$C_RESET" "$project"
  printf '%sRegion:%s  %s\n' "$C_BOLD" "$C_RESET" "$REGION"
  printf '\n'

  show_plan_classes

  if ((PLAN_TOTAL == 0)); then
    printf '%sNothing to delete%s %s- no Quake resources found for this project.%s\n' \
      "$C_DIM" "$C_RESET" "$C_DIM" "$C_RESET"
  else
    printf '\n%sTotal:%s %d resources\n' "$C_BOLD" "$C_RESET" "$PLAN_TOTAL"
  fi
}

# --- Deletion helpers --------------------------------------------------------

# Record a failure for the end-of-run summary. Does not exit.
record_failure() {
  FAILURES+=("$1")
  warn "$1"
}

# Log a phase announcement on stderr.
phase() {
  log "==> $1"
}

# Log one line per resource if --verbose, otherwise nothing (phase summary already printed).
resource_log() {
  if [[ "$VERBOSE" == "true" ]]; then
    log "  $1"
  fi
}

# --- Deletion phases ---------------------------------------------------------

delete_alarms() {
  local -a names=("$@")
  ((${#names[@]} == 0)) && return
  phase "Deleting CloudWatch alarms (${#names[@]})"
  if aws_cli cloudwatch delete-alarms --alarm-names "${names[@]}" 2>/dev/null; then
    local n
    for n in "${names[@]}"; do resource_log "deleted alarm $n"; done
  else
    record_failure "failed to delete CloudWatch alarms: ${names[*]}"
  fi
}

terminate_instances() {
  local -a ids=("$@")
  ((${#ids[@]} == 0)) && return
  phase "Terminating EC2 instances (${#ids[@]}) and waiting for termination"
  if ! aws_cli ec2 terminate-instances --instance-ids "${ids[@]}" >/dev/null 2>&1; then
    record_failure "failed to issue terminate-instances for: ${ids[*]}"
    return
  fi
  local n
  for n in "${ids[@]}"; do resource_log "terminating instance $n"; done
  if ! aws_cli ec2 wait instance-terminated --instance-ids "${ids[@]}" 2>/dev/null; then
    record_failure "timed out waiting for instances to terminate: ${ids[*]}"
  fi
}

# Fetch a single field of an ENI via JMESPath. Returns the empty string when
# the ENI is gone or the field is null (callers use is_absent_or_none).
_eni_field() {
  local eni="$1" path="$2"
  aws_cli ec2 describe-network-interfaces \
    --network-interface-ids "$eni" \
    --query "NetworkInterfaces[0].${path}" \
    --output text 2>/dev/null || true
}

# Poll an ENI's Status field until it is `available` (detach completed) or the
# ENI disappears. Returns the final status (possibly empty); records a failure
# and returns the last seen status if the timeout fires.
_wait_eni_available() {
  local eni="$1"
  local status attempts=0
  while true; do
    status="$(_eni_field "$eni" Status)"
    if is_absent_or_none "$status" || [[ "$status" == "available" ]]; then
      printf '%s' "$status"
      return
    fi
    attempts=$((attempts + 1))
    if ((attempts > 24)); then
      record_failure "eni $eni did not become available after ~2 min"
      printf '%s' "$status"
      return
    fi
    sleep 5
  done
}

delete_secondary_enis() {
  local -a ids=("$@")
  ((${#ids[@]} == 0)) && return
  phase "Deleting secondary ENIs (${#ids[@]})"
  local eni attachment_id status
  for eni in "${ids[@]}"; do
    status="$(_eni_field "$eni" Status)"
    if is_absent_or_none "$status"; then
      resource_log "eni $eni already gone"
      continue
    fi

    attachment_id="$(_eni_field "$eni" Attachment.AttachmentId)"
    if ! is_absent_or_none "$attachment_id"; then
      if ! aws_cli ec2 detach-network-interface --attachment-id "$attachment_id" --force >/dev/null 2>&1; then
        record_failure "failed to detach eni $eni (attachment $attachment_id)"
        continue
      fi
    fi

    status="$(_wait_eni_available "$eni")"
    if is_absent_or_none "$status"; then
      resource_log "eni $eni already gone"
      continue
    fi

    if aws_cli ec2 delete-network-interface --network-interface-id "$eni" 2>/dev/null; then
      resource_log "deleted eni $eni"
    else
      record_failure "failed to delete eni $eni"
    fi
  done
}

# Delete EBS volumes discovered by tag. Only volumes in the `available` state
# are deletable: `in-use` volumes are still attached to instances (they will
# vanish via delete_on_termination once those instances finish terminating),
# and any other state is transient. We re-fetch state per volume so this phase
# is idempotent if it runs after a partial cleanup.
delete_volumes() {
  local -a ids=("$@")
  ((${#ids[@]} == 0)) && return
  phase "Deleting EBS volumes (${#ids[@]})"
  local vol state
  for vol in "${ids[@]}"; do
    state="$(aws_cli ec2 describe-volumes \
      --volume-ids "$vol" \
      --query 'Volumes[0].State' \
      --output text 2>/dev/null || true)"
    if is_absent_or_none "$state"; then
      resource_log "volume $vol already gone"
      continue
    fi
    case "$state" in
      available)
        if aws_cli ec2 delete-volume --volume-id "$vol" 2>/dev/null; then
          resource_log "deleted volume $vol"
        else
          record_failure "failed to delete volume $vol"
        fi
        ;;
      in-use)
        resource_log "volume $vol still in-use; relying on delete_on_termination"
        ;;
      *)
        record_failure "volume $vol in unexpected state '$state'; not deleted"
        ;;
    esac
  done
}

# Revoke rules in one direction (ingress or egress) that reference other
# security groups via UserIdGroupPairs. Reads the permissions JSON from the
# matching SG field, then feeds it back to the matching revoke subcommand.
#
#   _revoke_sg_direction <sg-id> <direction>
#     direction is "ingress" or "egress"
_revoke_sg_direction() {
  local sg="$1" direction="$2"
  local field subcmd json
  case "$direction" in
    ingress) field="IpPermissions";       subcmd="revoke-security-group-ingress" ;;
    egress)  field="IpPermissionsEgress"; subcmd="revoke-security-group-egress"  ;;
  esac
  json="$(aws_cli ec2 describe-security-groups \
    --group-ids "$sg" \
    --query "SecurityGroups[0].${field}[?length(UserIdGroupPairs)>\`0\`]" \
    --output json 2>/dev/null || true)"
  if [[ -z "$json" || "$json" == "null" || "$json" == "[]" ]]; then
    return
  fi
  if aws_cli ec2 "$subcmd" --group-id "$sg" --ip-permissions "$json" >/dev/null 2>&1; then
    resource_log "revoked cross-SG ${direction} on $sg"
  else
    record_failure "failed to revoke ${direction} on $sg"
  fi
}

# Revoke all ingress/egress rules in the given SGs that reference other security
# groups (UserIdGroupPairs). CIDR-only rules are left alone; they vanish with the
# SG itself.
revoke_cross_sg_rules() {
  local -a sgs=("$@")
  ((${#sgs[@]} == 0)) && return
  phase "Revoking cross-SG rules across ${#sgs[@]} security groups"
  local sg
  for sg in "${sgs[@]}"; do
    _revoke_sg_direction "$sg" ingress
    _revoke_sg_direction "$sg" egress
  done
}

delete_route_tables() {
  local -a ids=("$@")
  ((${#ids[@]} == 0)) && return
  phase "Deleting route tables (${#ids[@]})"
  local rtb assoc_ids assoc
  for rtb in "${ids[@]}"; do
    read_lines assoc_ids < <(
      collect ec2 describe-route-tables \
        --route-table-ids "$rtb" \
        --query 'RouteTables[0].Associations[?Main!=`true`].RouteTableAssociationId'
    )
    for assoc in "${assoc_ids[@]}"; do
      if ! aws_cli ec2 disassociate-route-table --association-id "$assoc" >/dev/null 2>&1; then
        record_failure "failed to disassociate route-table association $assoc"
      fi
    done
    if aws_cli ec2 delete-route-table --route-table-id "$rtb" 2>/dev/null; then
      resource_log "deleted route table $rtb"
    else
      record_failure "failed to delete route table $rtb"
    fi
  done
}

# Delete each ID individually via `aws_cli <service> <subcmd> <flag> <id>`.
# Used for resource classes with a uniform per-item delete call.
#
#   delete_each LABEL SINGULAR SERVICE SUBCMD FLAG ids...
delete_each() {
  local label="$1" singular="$2" service="$3" subcmd="$4" flag="$5"
  shift 5
  local -a ids=("$@")
  ((${#ids[@]} == 0)) && return
  phase "Deleting ${label} (${#ids[@]})"
  local id
  for id in "${ids[@]}"; do
    if aws_cli "$service" "$subcmd" "$flag" "$id" 2>/dev/null; then
      resource_log "deleted ${singular} ${id}"
    else
      record_failure "failed to delete ${singular} ${id}"
    fi
  done
}

delete_igws() {
  local vpc="$1"
  shift
  local -a ids=("$@")
  ((${#ids[@]} == 0)) && return
  phase "Detaching and deleting internet gateways for VPC $vpc (${#ids[@]})"
  local igw
  for igw in "${ids[@]}"; do
    if ! aws_cli ec2 detach-internet-gateway --internet-gateway-id "$igw" --vpc-id "$vpc" >/dev/null 2>&1; then
      record_failure "failed to detach igw $igw from vpc $vpc"
      continue
    fi
    if aws_cli ec2 delete-internet-gateway --internet-gateway-id "$igw" 2>/dev/null; then
      resource_log "deleted igw $igw"
    else
      record_failure "failed to delete igw $igw"
    fi
  done
}

# --- Commands ----------------------------------------------------------------

cmd_list_summary() {
  local filter_user
  filter_user="$(resolve_effective_user)"

  local -a projects=() filtered=()
  read_lines projects < <(discover_all_projects)

  local p u
  for p in "${projects[@]}"; do
    u="$(user_from_project "$p")"
    if [[ "$u" == "$filter_user" ]]; then
      filtered+=("$p")
    fi
  done
  projects=("${filtered[@]}")

  log "Region: $REGION"
  log "User:   $filter_user"
  # When --user was passed explicitly, make it visible: read-only commands
  # (this summary, `list TESTNET`) deliberately do not enforce the cross-user
  # guardrail, but the user should know they are looking at someone else's
  # projects rather than their own.
  if [[ "$USER_OVERRIDE_PASSED" == "true" ]]; then
    local gh_sanitized
    gh_sanitized="$(sanitize "${GITHUB_USER:-}")"
    if [[ -n "$gh_sanitized" && "$gh_sanitized" != "$filter_user" ]]; then
      log "        (--user override; current GITHUB_USER resolves to '$gh_sanitized')"
    fi
  fi
  log ""

  if ((${#projects[@]} == 0)); then
    out "No Quake projects found for user $filter_user in $REGION."
    return 0
  fi

  # Group by testnet. Because we filter by user, each testnet maps to exactly
  # one project per user, so the grouping is one-project-per-testnet. The nested
  # layout still makes the testnet identity explicit without repeating the full
  # project name in the header.
  local p first=true testnet
  for p in "${projects[@]}"; do
    if [[ "$first" == "true" ]]; then
      first=false
    else
      out ""
    fi
    testnet="$(testnet_from_project "$p")"
    out "testnet: ${testnet}"
    out "  project: ${p}"
    discover_project "$p"
    show_plan_classes "    "
  done
}

cmd_list_detail() {
  local project_arg="$1"
  local effective_user project
  effective_user="$(resolve_effective_user)"
  project="$(project_name_from_arg "$project_arg" "$effective_user")"
  check_cross_user_guardrail "$project"

  discover_project "$project"
  show_plan "$project"
}

cmd_remove() {
  local project_arg="$1"
  local effective_user project
  effective_user="$(resolve_effective_user)"
  project="$(project_name_from_arg "$project_arg" "$effective_user")"
  check_cross_user_guardrail "$project"

  discover_project "$project"
  show_plan "$project"

  if ((PLAN_TOTAL == 0)); then
    return 0
  fi

  # Interactive confirmation unless --yes. The prompt IS the promotion.
  if [[ "$SKIP_PROMPT" != "true" ]]; then
    printf '\n' >&2
    local answer=""
    read -r -p "Delete these ${PLAN_TOTAL} resources? [y/N] " answer >&2 || true
    if [[ "$answer" != "y" && "$answer" != "Y" && "$answer" != "yes" ]]; then
      log "Aborted."
      return 0
    fi
  fi

  execute_deletion_plan
  emit_failure_summary
}

# Execute the 11-phase deletion plan for the globals populated by discover_project.
execute_deletion_plan() {
  # 1. CloudWatch alarm.
  delete_alarms "${ALARM_NAMES[@]}"

  # 2. EC2 instances (terminate + wait). `wait instance-terminated` blocks
  #    until volumes have either auto-deleted (delete_on_termination=true) or
  #    transitioned to `available`, so the volume phase below can run cleanly.
  terminate_instances "${INSTANCE_IDS[@]}"

  # 3. EBS volumes (only deletes those in `available` state; in-use volumes
  #    are left for delete_on_termination to clean up). Runs after instance
  #    termination so attached volumes have had a chance to detach.
  delete_volumes "${VOLUME_IDS[@]}"

  # 4. Secondary ENIs (force-detach, poll for available, delete).
  delete_secondary_enis "${ENI_IDS[@]}"

  # 5. Revoke cross-SG rules so SG deletion has no cross-references.
  revoke_cross_sg_rules "${SG_IDS[@]}"

  # 6. Route tables (disassociate non-main associations, then delete).
  delete_route_tables "${RTB_IDS[@]}"

  # 7. Subnets.
  delete_each "subnets" "subnet" ec2 delete-subnet --subnet-id "${SUBNET_IDS[@]}"

  # 8. Internet gateways (detach, then delete). Per-VPC because detach requires vpc-id.
  local vpc
  for vpc in "${VPC_IDS[@]}"; do
    local -a igws_for_vpc=()
    read_lines igws_for_vpc < <(
      collect ec2 describe-internet-gateways \
        --filters "Name=attachment.vpc-id,Values=${vpc}" \
        --query 'InternetGateways[].InternetGatewayId'
    )
    delete_igws "$vpc" "${igws_for_vpc[@]}"
  done

  # 9. Security groups.
  delete_each "security groups" "security group" ec2 delete-security-group --group-id "${SG_IDS[@]}"

  # 10. VPC.
  delete_each "VPCs" "vpc" ec2 delete-vpc --vpc-id "${VPC_IDS[@]}"

  # 11. Key pair.
  delete_each "key pairs" "key pair" ec2 delete-key-pair --key-name "${KEY_NAMES[@]}"
}

emit_failure_summary() {
  log ""
  if ((${#FAILURES[@]} == 0)); then
    log "Cleanup complete."
    return 0
  fi
  err "Cleanup completed with ${#FAILURES[@]} failure(s):"
  local f
  for f in "${FAILURES[@]}"; do
    err "  - $f"
  done
  exit 1
}

# --- Argument parsing --------------------------------------------------------

parse_args() {
  if (($# == 0)); then
    usage >&2
    exit 2
  fi
  case "${1:-}" in
    -h|--help) usage; exit 0 ;;
  esac

  SUBCOMMAND="$1"
  shift

  local -a positional=()
  while (($# > 0)); do
    case "$1" in
      --region)
        [[ $# -ge 2 ]] || die_usage "--region requires a value"
        REGION="$2"; shift 2 ;;
      --user)
        [[ $# -ge 2 ]] || die_usage "--user requires a value"
        USER_OVERRIDE="$2"; USER_OVERRIDE_PASSED=true; shift 2 ;;
      --yes|-y) SKIP_PROMPT=true; shift ;;
      --verbose) VERBOSE=true; shift ;;
      -h|--help) usage; exit 0 ;;
      --) shift; while (($# > 0)); do positional+=("$1"); shift; done ;;
      -*) die_usage "unknown option: $1" ;;
      *) positional+=("$1"); shift ;;
    esac
  done

  # Region precedence: --region > $AWS_REGION > us-east-1.
  if [[ -z "$REGION" ]]; then
    REGION="${AWS_REGION:-us-east-1}"
  fi

  case "$SUBCOMMAND" in
    list)
      if ((${#positional[@]} > 1)); then
        die_usage "list accepts at most one positional argument"
      fi
      if [[ "$SKIP_PROMPT" == "true" ]]; then
        die_usage "--yes is only valid with remove"
      fi
      if [[ "$VERBOSE" == "true" ]]; then
        die_usage "--verbose is only valid with remove"
      fi
      TESTNET_ARG="${positional[0]:-}"
      # Bare `list` (no positional) is the summary mode; an explicit empty
      # string would degenerate into a `arc--testnet-<user>` query.
      if ((${#positional[@]} == 1)) && [[ -z "$TESTNET_ARG" ]]; then
        die_usage "list TESTNET must be a non-empty value"
      fi
      ;;
    remove)
      if ((${#positional[@]} != 1)); then
        die_usage "remove requires exactly one positional argument (TESTNET)"
      fi
      TESTNET_ARG="${positional[0]}"
      if [[ -z "$TESTNET_ARG" ]]; then
        die_usage "remove TESTNET must be a non-empty value"
      fi
      ;;
    *)
      die_usage "unknown command: $SUBCOMMAND"
      ;;
  esac
}

# --- Entrypoint --------------------------------------------------------------

main() {
  parse_args "$@"
  require_cmd aws
  require_cmd git

  local repo_root
  repo_root="$(resolve_repo_root)"
  load_dotenv_if_needed "$repo_root"

  case "$SUBCOMMAND" in
    list)
      if [[ -z "$TESTNET_ARG" ]]; then
        cmd_list_summary
      else
        cmd_list_detail "$TESTNET_ARG"
      fi
      ;;
    remove)
      cmd_remove "$TESTNET_ARG"
      ;;
  esac
}

main "$@"
