// Runs the R package's tests in webR, in Node.js, with the package as
// webR's build of it, in the repository of R packages that rwasm made.
//
//   node tests/webr/run.mjs <rwasm's repository>
import { WebR } from 'webr';
import { readdirSync } from 'node:fs';
import { resolve, join } from 'node:path';

const repo = resolve(process.argv[2]);
const tests = resolve(import.meta.dirname, '../../r/tests');
const contrib = join(repo, 'bin/emscripten/contrib');
const version = readdirSync(contrib)[0];
const built = readdirSync(join(contrib, version)).find((f) => /^manby_.*\.tgz$/.test(f));

const webR = new WebR();
await webR.init();
for (const [dir, at] of [[repo, '/repo'], [tests, '/tests']]) {
  await webR.FS.mkdir(at);
  await webR.FS.mount('NODEFS', { root: dir }, at);
}
await webR.installPackages(['jsonlite', 'nanoarrow', 'processx', 'sf', 'testthat', 'withr'], { quiet: true });

const shelter = await new webR.Shelter();
const result = await shelter.captureR(`
  untar("/repo/bin/emscripten/contrib/${version}/${built}", exdir = .libPaths()[1])
  library(manby)
  results <- as.data.frame(testthat::test_dir(
    "/tests/testthat", package = "manby", load_package = "installed",
    reporter = "summary", stop_on_failure = FALSE
  ))
  c(failed = sum(results$failed) + sum(results$error), skipped = sum(results$skipped),
    passed = sum(results$nb) - sum(results$failed) - sum(results$skipped))
`);
for (const out of result.output) {
  if (out.type === 'stdout' || out.type === 'stderr') console.log(out.data);
}
const counts = await result.result.toJs();
const [failed, skipped, passed] = counts.values;
console.log(`webR: ${passed} passed, ${skipped} skipped, ${failed} failed`);
await shelter.purge();
webR.close();
process.exit(failed === 0 && passed > 0 ? 0 : 1);
