import { describe, expect, it } from "vitest"
import { extractSessionIdFromLabel, extractSessionIdFromSearch } from "./simulator-window"

describe("extractSessionIdFromLabel", () => {
  it("decodes the hex-encoded session id used by simulator windows", () => {
    expect(extractSessionIdFromLabel("simulator-616e64726f69643a2a2a2a356433")).toBe(
      "android:***5d3",
    )
  })

  it("supports UTF-8 session ids", () => {
    expect(extractSessionIdFromLabel("simulator-616e64726f69643ae8aebee5a487")).toBe("android:设备")
  })

  it.each([
    "main",
    "simulator-",
    "simulator-abc",
    "simulator-not-hex",
    "simulator-ff",
  ])("returns null for an invalid simulator label: %s", (label) => {
    expect(extractSessionIdFromLabel(label)).toBeNull()
  })

  it("extracts a session id from the simulator URL route", () => {
    expect(extractSessionIdFromSearch("?simulator=616e64726f69643a4142")).toBe("android:AB")
    expect(extractSessionIdFromSearch("?simulator=not-hex")).toBeNull()
    expect(extractSessionIdFromSearch("?other=value")).toBeNull()
  })
})
