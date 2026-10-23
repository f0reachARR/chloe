// 結果を、ほかの VM を使う確認と同じ形 ("==RESULT== 名前: ok|FAIL ...") で出す
// (images/guest/dev/test-all.sh が数える)。
import { stripVTControlCharacters } from 'node:util';

import type { Reporter, TestCase, TestResult } from '@playwright/test/reporter';

export default class ResultReporter implements Reporter {
  onTestEnd(test: TestCase, result: TestResult) {
    if (result.status === 'skipped') {
      return;
    }
    const project = test.parent.project()?.name ?? '';
    const name = `web-${project} ${test.title}`;
    const secs = (result.duration / 1000).toFixed(1);
    if (result.status === 'passed') {
      process.stdout.write(`==RESULT== ${name}: ok (+${secs}s)\n`);
    } else {
      const message = stripVTControlCharacters(result.error?.message ?? result.status).split(
        '\n',
      )[0];
      process.stdout.write(`==RESULT== ${name}: FAIL ${message} (+${secs}s)\n`);
    }
  }
}
