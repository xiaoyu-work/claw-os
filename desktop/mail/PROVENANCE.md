# Mail Source Provenance

## Thunderbird import

`comm/` is a complete source copy from
[thunderbird/thunderbird-desktop](https://github.com/thunderbird/thunderbird-desktop),
release **153.2.0esr**, tag `THUNDERBIRD_153_2_0esr_RELEASE`, commit
`3e90aed10e56ece77b9a5cd2e133df443b13839d`.

The initial imported tree is `958d4f25021ea8600952a05070d6f447924c2ec8`:
41,094 tracked files, preserving upstream paths, executable modes, symlinks,
copyright notices, third-party sources and tests. The import uses `git archive`
of the pinned commit; it is not a submodule, a downloaded binary, or a selection
of extension APIs. Future product modifications are ordinary repository diffs.

## Matching platform

Thunderbird's [`.gecko_rev.yml`](comm/.gecko_rev.yml) requires Mozilla ESR153
revision `92c5bf513a3e4e39fa70df2df6a565a1049a9920`, identified upstream as
`FIREFOX_153_2_0esr_BUILD1`. The corresponding
[Firefox Git repository](https://github.com/mozilla-firefox/firefox) tag resolves
to `feec67e62a5148b41fd017ccbbc463e8a6f9e83d`.

Both revision forms are recorded in [upstream.json](upstream.json). Never
substitute Firefox main, the host's installed Firefox, or an independently
chosen ESR release. The platform remains a source build dependency; the Mail
product source itself lives here.

## Licenses and trademarks

Thunderbird is principally MPL-2.0, with separately licensed third-party
components. Preserve individual file notices and the complete contents of
[`comm/other-licenses/`](comm/other-licenses/) and
[`comm/third_party/`](comm/third_party/). The matching Firefox platform also
provides its `LICENSE` and `toolkit/content/license.html`; those notices and
license texts are required inputs to binary distribution, not optional build
files. See the upstream [licensing policy](https://www.mozilla.org/MPL/).

Source licenses do not grant Thunderbird or Mozilla trademark rights.
This import retains original source branding for provenance; it does not
authorize distributing a modified product as an official Thunderbird release.
Product branding, corresponding-source publication and packaged license
notices must be completed before distributing the fork.

## Security maintenance

This is a maintained fork, not a one-time frozen dependency. Track Thunderbird
and Firefox ESR security releases together, record each imported revision,
preserve local product changes, and rebuild the matched pair. Publish the
corresponding source and preserve upstream attribution when shipping binaries.
Until the replacement package and update path are ready, installed systems
continue using their existing packaged Thunderbird; this source import does
not disable its updates or replace a user's profile.
