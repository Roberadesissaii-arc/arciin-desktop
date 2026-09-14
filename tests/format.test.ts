import { describe, expect, it } from "vitest"

import {
  formatCode,
  hostLabel,
  isCompleteCode,
  maskAddress,
  normalizeCode,
} from "../src/lib/format"
import { isAppError, toAppError } from "../src/types"

describe("pairing codes", () => {
  it("keeps only digits, however the code was copied", () => {
    expect(normalizeCode("482731")).toBe("482731")
    expect(normalizeCode("482 731")).toBe("482731")
    expect(normalizeCode("482-731")).toBe("482731")
    expect(normalizeCode(" 482 731 ")).toBe("482731")
  })

  it("never exceeds six digits", () => {
    expect(normalizeCode("4827319999")).toBe("482731")
  })

  it("drops non-numeric input entirely", () => {
    expect(normalizeCode("abcdef")).toBe("")
  })

  it("groups the display the way the server's Settings page does", () => {
    expect(formatCode("482731")).toBe("482 731")
    expect(formatCode("48")).toBe("48")
    expect(formatCode("4827")).toBe("482 7")
  })

  it("only reports completeness at exactly six digits", () => {
    expect(isCompleteCode("48273")).toBe(false)
    expect(isCompleteCode("482731")).toBe(true)
    expect(isCompleteCode("482 731")).toBe(true)
    expect(isCompleteCode("")).toBe(false)
  })
})

describe("address labels", () => {
  it("hides the scheme for a plain LAN address", () => {
    expect(hostLabel("http://192.168.1.50")).toBe("192.168.1.50")
  })

  it("keeps a non-default port", () => {
    expect(hostLabel("http://192.168.1.50:3000")).toBe("192.168.1.50:3000")
  })

  it("keeps https, because it is information", () => {
    expect(hostLabel("https://arciin.example.com")).toBe("https://arciin.example.com")
    expect(hostLabel("https://arciin.example.com:8443")).toBe(
      "https://arciin.example.com:8443",
    )
  })

  it("falls back to the raw value rather than throwing", () => {
    expect(hostLabel("not a url")).toBe("not a url")
  })
})

describe("error normalization", () => {
  it("passes a native error through unchanged", () => {
    const native = {
      code: "PAIRING_CODE_EXPIRED",
      message: "That pairing code expired.",
      retryable: true,
    }
    expect(toAppError(native)).toEqual(native)
    expect(isAppError(native)).toBe(true)
  })

  it("wraps anything else so no screen sees a bare throw", () => {
    for (const thrown of ["boom", null, undefined, 42, new Error("x"), {}]) {
      const normalized = toAppError(thrown)
      expect(normalized.code).toBe("INTERNAL_ERROR")
      expect(typeof normalized.message).toBe("string")
      expect(normalized.message.length).toBeGreaterThan(0)
    }
  })

  it("does not mistake a partial object for a native error", () => {
    expect(isAppError({ code: "X" })).toBe(false)
    expect(isAppError({ message: "X" })).toBe(false)
  })
})

describe("address masking", () => {
  it("hides the final octet but keeps the subnet and port", () => {
    expect(maskAddress("192.168.1.50:3002")).toBe("192.168.1.xxx:3002")
    expect(maskAddress("10.0.0.7:80")).toBe("10.0.0.xxx:80")
  })

  it("works without a port", () => {
    expect(maskAddress("192.168.1.50")).toBe("192.168.1.xxx")
  })

  it("leaves hostnames alone", () => {
    // A hostname carries no per-machine address to hide, and masking part of
    // it would make it unrecognisable rather than private.
    expect(maskAddress("arciin.local")).toBe("arciin.local")
    expect(maskAddress("https://arciin.example.com")).toBe("https://arciin.example.com")
    expect(maskAddress("arciin.example.com:8443")).toBe("arciin.example.com:8443")
  })

  it("never leaks the octet it is meant to hide", () => {
    expect(maskAddress("192.168.1.50:3002")).not.toContain("40:")
    expect(maskAddress("192.168.1.123")).not.toContain("123")
  })

  it("composes with hostLabel, which is how the card renders it", () => {
    expect(maskAddress(hostLabel("http://192.168.1.50:3002"))).toBe("192.168.1.xxx:3002")
    expect(maskAddress(hostLabel("http://192.168.1.50"))).toBe("192.168.1.xxx")
  })
})
