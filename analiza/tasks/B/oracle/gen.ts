import semver from "semver";
const ranges = ["^1.2.3","^0.2.3","^0.0.3","^1.2.x","^0.0.x","^0.x","^0","^1.2.3-beta.2","^0.0.3-beta","~1.2.3","~1.2","~1","~0.2.3","~1.2.3-beta.2",
"1.2.3 - 2.3.4","1.2 - 2.3.4","1.2.3 - 2.3","1.2.3 - 2","*","x","1.x","1.2.*","1","1.2","",">=1.2.3 <2.0.0",">1.2.3","<1.2.3","<=1.2.3","=1.2.3","1.2.3",
">=1.2.3-alpha.3 <1.2.4","1.2.3-beta.2 || >=2.0.0-rc.1","<1.0.0 || >=2.3.1 <2.4.5 || >=2.5.2 <3.0.0",">1.x","<1.x","<=1.2",">=1.0.0-0 <2.0.0","~1.2.3+build","^1.2.3+build",
"^ 1.2.3","~> 1.2","1.2.3 - 2.3.4 || ^3","invalid range","1.2.3.4",">=x","<x",">01.2.3","1.2.3-beta.10 - 1.2.3-beta.2", "^1.2.3-0", "<2.0.0-0"];
const versions = ["0.0.1","0.0.3","0.0.4","0.0.3-beta","0.0.3-beta.2","0.1.0","0.2.3","0.2.4","0.3.0","1.0.0","1.0.0-0","1.2.0","1.2.2","1.2.3","1.2.3-alpha.3","1.2.3-alpha.10","1.2.3-beta","1.2.3-beta.2","1.2.3-beta.10","1.2.3-beta.11","1.2.3-beta.x","1.2.3-rc.1+b.5","1.2.3+build.1","1.2.4","1.2.4-beta","1.3.0","1.9.0","1.10.0","2.0.0","2.0.0-0","2.0.0-rc.1","2.0.0-rc.2","2.3.4","2.3.5","2.4.0","2.5.2","3.0.0","3.1.0-alpha","01.2.3","1.2","v1.2.3","1.2.3.4","not-a-version"];
const sat: [string,string,boolean][] = [];
for (const r of ranges) for (const v of versions) sat.push([v, r, semver.satisfies(v, r)]);
const sets = [["1.9.0","1.10.0","2.0.0"],["1.2.3-beta.2","1.2.3-beta.10","1.2.3"],["0.0.3","0.0.3-beta","0.0.4"],["2.0.0-rc.1","2.0.0-rc.2","1.99.0"],["1.2.3+build.9","1.2.3+build.1","1.2.2"],["3.0.0-alpha.beta","3.0.0-alpha.1","3.0.0-alpha"],["bad","1.0.0"]];
const max: [string[],string,string|null][] = [];
for (const r of ranges) for (const s of sets) max.push([s, r, semver.maxSatisfying(s, r)]);
await Bun.write(new URL("../cases.json", import.meta.url), JSON.stringify({ sat, max }));
console.log(sat.length, max.length, sat.filter(x=>x[2]).length);
