import { expect, test } from "bun:test";
import { pointRecord, zoneFile } from "./dns";

test("a link domain zone has the TXT record and a DNS-only CNAME to Owlee", () => {
  const records = [
    { name: "_owlee-verify.go.brand.com", type: "TXT" as const, value: "owlee-verify=abc_123" },
    pointRecord("go.brand.com", "https://status.example.com"),
  ];
  expect(zoneFile(records)).toBe(
    "; DNS records for Owlee. Import this file at your DNS host.\n" +
      '_owlee-verify.go.brand.com.\t3600\tIN\tTXT\t"owlee-verify=abc_123"\n' +
      "go.brand.com.\t3600\tIN\tCNAME\tstatus.example.com. ; cf_tags=cf-proxied:false\n",
  );
});

test("an origin on an IP address gets an address record", () => {
  expect(pointRecord("go.brand.com", "http://203.0.113.9:8080")).toEqual({ name: "go.brand.com", type: "A", value: "203.0.113.9" });
  expect(pointRecord("go.brand.com", "http://[2001:db8::1]")).toEqual({ name: "go.brand.com", type: "AAAA", value: "2001:db8::1" });
});

test("quotes in a TXT value are escaped", () => {
  expect(zoneFile([{ name: "a.test.", type: "TXT", value: 'say "hi"' }])).toContain('"say \\"hi\\""');
});
