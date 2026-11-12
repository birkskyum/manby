// Runs the R package's tests in webR, in Node.js, with webR's build of the
// package that rwasm made.
//
//   node tests/webr/run.mjs <manby_*.tgz>
import { WebR } from 'webr';
import { resolve, dirname, basename } from 'node:path';

const built = resolve(process.argv[2]);
const tests = resolve(import.meta.dirname, '../../r/tests');

const webR = new WebR();
await webR.init();
for (const [dir, at] of [[dirname(built), '/built'], [tests, '/tests']]) {
  await webR.FS.mkdir(at);
  await webR.FS.mount('NODEFS', { root: dir }, at);
}
await webR.installPackages(['dbplyr', 'dplyr', 'duckdb', 'jsonlite', 'nanoarrow', 'processx', 'sf', 'testthat', 'withr'], { quiet: true });

const shelter = await new webR.Shelter();
const result = await shelter.captureR(`
  untar("/built/${basename(built)}", exdir = .libPaths()[1], tar = "internal")
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
