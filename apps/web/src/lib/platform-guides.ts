export type GuideCode = { file?: string; code: string };

export type PlatformGuide = {
  label: string;
  sdk: string;
  docs: string;
  install: GuideCode;
  configure: (dsn: string) => GuideCode[];
  /** Absent when there is no SDK to name; the page always offers a curl test. */
  test?: GuideCode;
};

/* Keys match normalize_platform in apps/server/src/errors/store.rs. */
export const platformGuides = {
  php: {
    label: "PHP",
    sdk: "sentry/sentry",
    docs: "https://docs.sentry.io/platforms/php/",
    install: { code: "composer require sentry/sentry" },
    configure: (dsn) => [{ file: "Run as early as possible, before your app boots", code: `\\Sentry\\init([\n    'dsn' => '${dsn}',\n]);` }],
    test: { code: "\\Sentry\\captureException(new \\Exception('Owlee test error'));" },
  },
  laravel: {
    label: "Laravel",
    sdk: "sentry/sentry-laravel",
    docs: "https://docs.sentry.io/platforms/php/guides/laravel/",
    install: { code: "composer require sentry/sentry-laravel" },
    configure: (dsn) => [
      { file: "Writes config/sentry.php and SENTRY_LARAVEL_DSN to .env", code: `php artisan sentry:publish --dsn=${dsn}` },
      {
        file: "bootstrap/app.php, inside the existing withExceptions call",
        code: "use Sentry\\Laravel\\Integration;\n\n->withExceptions(function (Exceptions $exceptions) {\n    Integration::handles($exceptions);\n})",
      },
    ],
    test: { code: "php artisan sentry:test" },
  },
  javascript: {
    label: "Browser JavaScript",
    sdk: "@sentry/browser",
    docs: "https://docs.sentry.io/platforms/javascript/",
    install: { code: "npm install @sentry/browser --save" },
    configure: (dsn) => [
      { file: "Your entry file, before other imports", code: `import * as Sentry from "@sentry/browser";\n\nSentry.init({\n  dsn: "${dsn}",\n});` },
    ],
    test: {
      file: "Errors thrown from the devtools console are not captured, so trigger it from the page",
      code: `<button onclick="throw new Error('Owlee test error')">Break the world</button>`,
    },
  },
  node: {
    label: "Node.js",
    sdk: "@sentry/node",
    docs: "https://docs.sentry.io/platforms/javascript/guides/node/",
    install: { code: "npm install @sentry/node --save" },
    configure: (dsn) => [
      { file: "instrument.js", code: `const Sentry = require("@sentry/node");\n\nSentry.init({\n  dsn: "${dsn}",\n});` },
      { file: "Load it before your app", code: "node --import ./instrument.js app.js" },
    ],
    test: { file: "app.js", code: `setTimeout(() => {\n  throw new Error("Owlee test error");\n}, 99);` },
  },
  python: {
    label: "Python",
    sdk: "sentry-sdk",
    docs: "https://docs.sentry.io/platforms/python/",
    install: { code: 'pip install "sentry-sdk"' },
    configure: (dsn) => [{ file: "Your entry point", code: `import sentry_sdk\n\nsentry_sdk.init(\n    dsn="${dsn}",\n)` }],
    test: { code: "division_by_zero = 1 / 0" },
  },
  rust: {
    label: "Rust",
    sdk: "sentry",
    docs: "https://docs.sentry.io/platforms/rust/",
    install: { code: "cargo add sentry" },
    configure: (dsn) => [
      {
        file: "main.rs, before any async runtime starts",
        code: `let _guard = sentry::init(\n    sentry::ClientOptions::new()\n        .dsn("${dsn}")\n        .maybe_release(sentry::release_name!()),\n);`,
      },
    ],
    test: { code: `panic!("Owlee test error");` },
  },
  go: {
    label: "Go",
    sdk: "sentry-go",
    docs: "https://docs.sentry.io/platforms/go/",
    install: { code: "go get github.com/getsentry/sentry-go" },
    configure: (dsn) => [
      {
        file: "main.go",
        code: `err := sentry.Init(sentry.ClientOptions{\n    Dsn: "${dsn}",\n})\nif err != nil {\n    log.Fatalf("sentry.Init: %s", err)\n}\ndefer sentry.Flush(2 * time.Second)`,
      },
    ],
    test: { code: `sentry.CaptureException(errors.New("Owlee test error"))` },
  },
  java: {
    label: "Java",
    sdk: "io.sentry:sentry",
    docs: "https://docs.sentry.io/platforms/java/",
    install: { file: "build.gradle", code: `implementation 'io.sentry:sentry:8.65.0'` },
    configure: (dsn) => [
      { code: `import io.sentry.Sentry;\n\nSentry.init(options -> {\n  options.setDsn("${dsn}");\n});` },
    ],
    test: {
      code: `try {\n  throw new Exception("Owlee test error");\n} catch (Exception e) {\n  Sentry.captureException(e);\n}`,
    },
  },
  ruby: {
    label: "Ruby",
    sdk: "sentry-ruby",
    docs: "https://docs.sentry.io/platforms/ruby/",
    install: { code: "bundle add sentry-ruby" },
    configure: (dsn) => [{ code: `require 'sentry-ruby'\n\nSentry.init do |config|\n  config.dsn = '${dsn}'\nend` }],
    test: {
      code: "begin\n  1 / 0\nrescue ZeroDivisionError => exception\n  Sentry.capture_exception(exception)\nend",
    },
  },
  dotnet: {
    label: ".NET",
    sdk: "Sentry",
    docs: "https://docs.sentry.io/platforms/dotnet/",
    install: { code: "dotnet add package Sentry" },
    configure: (dsn) => [{ file: "Program.cs, first thing at startup", code: `SentrySdk.Init(options =>\n{\n    options.Dsn = "${dsn}";\n});` }],
    test: {
      code: `SentrySdk.CaptureException(new Exception("Owlee test error"));\nawait SentrySdk.FlushAsync();`,
    },
  },
  other: {
    label: "Other",
    sdk: "Any Sentry SDK",
    docs: "https://docs.sentry.io/platforms/",
    install: { file: "Pick the official SDK for your language", code: "https://docs.sentry.io/platforms/" },
    configure: (dsn) => [{ file: "Most SDKs read the DSN from this variable", code: `SENTRY_DSN=${dsn}` }],
  },
} satisfies Record<string, PlatformGuide>;

export type PlatformId = keyof typeof platformGuides;

export const platformIds = Object.keys(platformGuides) as PlatformId[];

export function isPlatformId(value: string): value is PlatformId {
  return value in platformGuides;
}

/* The server keeps unknown platform strings as-is, so fall back to Other. */
export function guideFor(platform: string): PlatformGuide {
  return isPlatformId(platform) ? platformGuides[platform] : platformGuides.other;
}
