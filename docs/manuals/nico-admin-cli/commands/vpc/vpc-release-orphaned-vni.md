# `nico-admin-cli vpc release-orphaned-vni`

_[Network commands](../../network.md) › [vpc](./vpc.md) › **release-orphaned-vni**_

## NAME

nico-admin-cli-vpc-release-orphaned-vni - Release the orphaned VNI owned
by a soft-deleted VPC

## SYNOPSIS

**nico-admin-cli vpc release-orphaned-vni** \[**--if-version-match**\]
\<**--expected-vni**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*ID*\>

## DESCRIPTION

Release the one VNI allocation owned by an already soft-deleted VPC.
Core requires the VPC to be soft-deleted, the exact persisted VNI, and
no remaining dependencies, and fails on missing, duplicate, or
inconsistent allocations. The soft-deleted VPCs version does not
advance.

Requires --cloud-unsafe-op USERNAME before vpc. Before release, the CLI
reads the deleted VPCs persisted state and asks for fresh confirmation
that the displayed allocation matches the requested VNI. Both stdin and
stderr must be terminals. Scripts must supply --if-version-match. Each
invocation without a version approves a new action; it does not resume
an earlier attempt.

Before mutation, the observed routing state is printed as JSON to
stderr. Each RPC attempt uses the client request timeout (300 seconds by
default, configurable with FORGE_CLIENT_REQUEST_TIMEOUT_SECS), including
connection setup and response reads. This command does not retry
mutations. A soft-deleted VPCs version does not advance, so a repeated
request with the same --if-version-match fails safely once the
allocation is released.

Use --format ascii-table (default), json, or yaml and --output PATH
before vpc. CSV output is unsupported.

## OPTIONS

**--if-version-match** *\<VERSION\>*  
Version observed with the orphaned VNI; required for scripts, omitted
for interactive confirmation; keep it unchanged when rerunning this
request

**--expected-vni** *\<EXPECTED_VNI\>*  
Exact orphaned VNI observed with this version (1..=16777215); keep it
unchanged when rerunning this request

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

\<*ID*\>  
VPC ID whose orphaned allocation will be released

## Examples

```sh
nico-admin-cli --cloud-unsafe-op admin vpc release-orphaned-vni 12345678-1234-5678-90ab-cdef01234567 --expected-vni 7000
nico-admin-cli --cloud-unsafe-op admin vpc release-orphaned-vni 12345678-1234-5678-90ab-cdef01234567 --if-version-match V1-T1789080000000000 --expected-vni 7000
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
