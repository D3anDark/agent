# Version ranges

Implement `satisfies` and `maxSatisfying` in `src/semver.ts` with the same results as
the npm package `semver` (node-semver v7) with default options: not loose, prereleases
excluded unless a comparator in the same comparator set has a prerelease on the same
`major.minor.patch`.

Support the full range grammar of node-semver: primitive comparators (`<`, `<=`, `>`,
`>=`, `=`, bare version), `||`, space-separated AND sets, hyphen ranges (`1.2 - 2.3.4`),
X-ranges (`*`, `x`, `X`, `1.x`, `1.2.*`, partial versions like `1` or `1.2`), tilde
ranges (`~1.2.3`, `~1.2`, `~1`, `~0.2.3`) and caret ranges (`^1.2.3`, `^0.2.3`,
`^0.0.3`, `^1.2.x`, `^0.0.x`, `^0.x`), with or without prereleases and build metadata.
Precedence follows SemVer 2.0.0 (prerelease identifiers compared numerically when
numeric, numeric ones lower than alphanumeric ones, build metadata ignored).

An invalid version makes `satisfies` return `false`. An invalid range makes `satisfies`
return `false` and `maxSatisfying` return `null`. `maxSatisfying` returns the highest
version in the list that satisfies the range, or `null`.

Do not add dependencies; write the implementation yourself.
