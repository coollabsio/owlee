import type { GuideCode } from "@/lib/platform-guides";

/** Owlee's OTLP/HTTP base URL. SDKs append `/v1/logs`, `/v1/traces`, and `/v1/metrics`. */
export const otlpEndpoint = (origin: string) => `${origin}/otlp`;

/** The header value is percent-encoded, as OTEL_EXPORTER_OTLP_HEADERS requires, so the space is `%20`. */
export function otelEnv(origin: string, token: string) {
  return [
    `OTEL_EXPORTER_OTLP_ENDPOINT="${otlpEndpoint(origin)}"`,
    `OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer%20${token}"`,
  ];
}

export function collectorConfig(origin: string, token: string) {
  return `receivers:
  otlp:
    protocols:
      grpc: { endpoint: 0.0.0.0:4317 }
      http: { endpoint: 0.0.0.0:4318 }
processors:
  batch: {}
exporters:
  otlphttp:
    endpoint: ${otlpEndpoint(origin)}
    headers:
      Authorization: "Bearer ${token}"
service:
  pipelines:
    logs: { receivers: [otlp], processors: [batch], exporters: [otlphttp] }
    traces: { receivers: [otlp], processors: [batch], exporters: [otlphttp] }
    metrics: { receivers: [otlp], processors: [batch], exporters: [otlphttp] }`;
}

export type SdkGuide = { id: string; label: string; docs: string; install: GuideCode; code: GuideCode };

/** Each SDK reads the endpoint and the token from the two environment variables, so the code holds neither. */
export const sdkGuides: SdkGuide[] = [
  {
    id: "node",
    label: "Node.js",
    docs: "https://github.com/open-telemetry/opentelemetry-js/tree/main/experimental/packages/exporter-logs-otlp-http",
    install: { code: "npm i @opentelemetry/api @opentelemetry/sdk-node @opentelemetry/sdk-logs @opentelemetry/exporter-logs-otlp-http" },
    code: {
      file: "otel.mjs (run with node --import ./otel.mjs app.mjs)",
      code: `import { NodeSDK } from "@opentelemetry/sdk-node";
import { BatchLogRecordProcessor } from "@opentelemetry/sdk-logs";
import { OTLPLogExporter } from "@opentelemetry/exporter-logs-otlp-http";
import { logs, SeverityNumber } from "@opentelemetry/api";

const sdk = new NodeSDK({
  logRecordProcessors: [new BatchLogRecordProcessor({ exporter: new OTLPLogExporter() })],
});
sdk.start();
process.on("SIGTERM", () => sdk.shutdown().then(() => process.exit(0)));

logs.getLogger("app").emit({ severityNumber: SeverityNumber.INFO, severityText: "INFO", body: "Hello from Node.js" });`,
    },
  },
  {
    id: "python",
    label: "Python",
    docs: "https://opentelemetry-python.readthedocs.io/en/latest/sdk/_logs.html",
    install: { code: "pip install opentelemetry-sdk opentelemetry-exporter-otlp-proto-http opentelemetry-instrumentation-logging" },
    code: {
      file: "main.py",
      code: `import logging
from opentelemetry._logs import set_logger_provider
from opentelemetry.sdk._logs import LoggerProvider
from opentelemetry.sdk._logs.export import BatchLogRecordProcessor
from opentelemetry.exporter.otlp.proto.http._log_exporter import OTLPLogExporter
from opentelemetry.instrumentation.logging import LoggingInstrumentor

provider = LoggerProvider()
provider.add_log_record_processor(BatchLogRecordProcessor(OTLPLogExporter()))
set_logger_provider(provider)
LoggingInstrumentor().instrument()
logging.getLogger().setLevel(logging.INFO)

logging.getLogger("app").info("Hello from Python")
provider.shutdown()`,
    },
  },
  {
    id: "go",
    label: "Go",
    docs: "https://pkg.go.dev/go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploghttp",
    install: {
      code: "go get go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploghttp go.opentelemetry.io/otel/sdk/log go.opentelemetry.io/otel/log/global go.opentelemetry.io/contrib/bridges/otelslog",
    },
    code: {
      file: "main.go",
      code: `package main

import (
	"context"
	"log/slog"

	"go.opentelemetry.io/contrib/bridges/otelslog"
	"go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploghttp"
	"go.opentelemetry.io/otel/log/global"
	sdklog "go.opentelemetry.io/otel/sdk/log"
)

func main() {
	ctx := context.Background()
	exporter, err := otlploghttp.New(ctx)
	if err != nil {
		panic(err)
	}
	provider := sdklog.NewLoggerProvider(sdklog.WithProcessor(sdklog.NewBatchProcessor(exporter)))
	defer provider.Shutdown(ctx)
	global.SetLoggerProvider(provider)

	slog.SetDefault(otelslog.NewLogger("app"))
	slog.Info("Hello from Go")
}`,
    },
  },
  {
    id: "rust",
    label: "Rust",
    docs: "https://docs.rs/opentelemetry-appender-tracing/latest/opentelemetry_appender_tracing/",
    install: {
      code: `cargo add opentelemetry opentelemetry_sdk opentelemetry-appender-tracing tracing tracing-subscriber
cargo add opentelemetry-otlp --features logs,http-proto,reqwest-blocking-client,reqwest-rustls`,
    },
    code: {
      file: "main.rs",
      code: `use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, Protocol, WithExportConfig};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use tracing_subscriber::{filter::LevelFilter, prelude::*};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let exporter = LogExporter::builder().with_http().with_protocol(Protocol::HttpBinary).build()?;
    let provider = SdkLoggerProvider::builder().with_batch_exporter(exporter).build();
    tracing_subscriber::registry()
        .with(OpenTelemetryTracingBridge::new(&provider).with_filter(LevelFilter::INFO))
        .init();

    tracing::info!("Hello from Rust");
    provider.shutdown()?;
    Ok(())
}`,
    },
  },
];
