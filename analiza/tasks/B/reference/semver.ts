import semver from "/tmp/hb/oracle/node_modules/semver/index.js";
export const satisfies = (v: string, r: string) => semver.satisfies(v, r);
export const maxSatisfying = (vs: string[], r: string) => semver.maxSatisfying(vs, r);
