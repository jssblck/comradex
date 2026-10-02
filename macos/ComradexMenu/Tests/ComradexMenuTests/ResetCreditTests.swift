import AppKit
import XCTest
@testable import ComradexMenu

private func resetFixture() throws -> UIStatusSnapshot {
    try JSONDecoder().decode(UIStatusSnapshot.self, from: Data(#"""
    {"daemon_running":true,"accounts":[{"name":"personal","kind":"codex_home","signed_in":true,"auth_state":"signed_in","usage_percent":100,"reset_credits":{"available_count":3,"observed_at_unix":1790900000,"credits":[
      {"id":"one","reset_type":"codex_rate_limits","status":"available","granted_at":"2026-10-01T01:02:03.456Z","expires_at":"2100-01-02T03:04:05.678+02:00","title":"Full reset"},
      {"id":"expired","reset_type":"codex_rate_limits","status":"available","granted_at":"2025-01-01T00:00:00Z","expires_at":"2025-02-01T00:00:00Z","title":"Expired reset"},
      {"id":"future","reset_type":"future_type","status":"available","granted_at":"2026-10-01T00:00:00Z","expires_at":null,"title":"Future reset"}
    ]}}],"pools":[{"name":"default","members":["personal"]}]}
    """#.utf8))
}

final class ResetCreditTests: XCTestCase {
    @MainActor
    func testResetSubmenuListsDirectActionsWithPreciseExpiry() throws {
        let snapshot = try resetFixture()
        let store = ComradexStore(client: ResetRecordingClient(snapshot: snapshot))
        store.apply(status: snapshot)
        let controller = MenuBarController(store: store)
        controller.rebuildMenu()
        let row = try XCTUnwrap(controller.renderedMenu.items.first { $0.title.hasPrefix("personal ·") })
        XCTAssertFalse(row.title.contains("resets"))
        let accountMenu = try XCTUnwrap(row.submenu)
        let resetItems = accountMenu.items.filter { $0.title == "2 resets available" }
        XCTAssertEqual(resetItems.count, 1)
        let credits = try XCTUnwrap(resetItems.first?.submenu)
        XCTAssertEqual(credits.items.count, 2)
        let available = try XCTUnwrap(credits.items.first { $0.title.hasPrefix("Full reset ·") })
        XCTAssertEqual(available.title, "Full reset · Expires 2100-01-02T03:04:05.678+02:00")
        XCTAssertTrue(available.isEnabled)
        XCTAssertNil(available.submenu)
        XCTAssertEqual(available.action, NSSelectorFromString("useResetSelected:"))
        XCTAssertTrue(available.target === controller)
        XCTAssertFalse(credits.items.contains { $0.title.hasPrefix("Expired reset") })
        let unsupported = try XCTUnwrap(credits.items.first { $0.title.hasPrefix("Future reset ·") })
        XCTAssertFalse(unsupported.isEnabled)
        XCTAssertNil(unsupported.submenu)
        let credit = try XCTUnwrap(snapshot.accounts.first?.resetCredits?.credits?.first)
        let alert = MenuBarController.resetConfirmation(account: "personal", credit: credit)
        XCTAssertEqual(alert.buttons.first?.title, "Cancel")
        XCTAssertEqual(alert.buttons.last?.title, "Use Reset")
        XCTAssertTrue(alert.informativeText.contains(credit.expiryDescription))
        XCTAssertTrue(alert.messageText.contains("personal"))
    }

    func testExpiryBoundaryAndInvalidDates() throws {
        let credit = try XCTUnwrap(resetFixture().accounts.first?.resetCredits?.credits?.first)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let expiry = try XCTUnwrap(formatter.date(from: credit.expiresAt!))
        XCTAssertTrue(credit.canRedeem(at: expiry.addingTimeInterval(-0.001)))
        XCTAssertFalse(credit.canRedeem(at: expiry))
        let invalid = ResetCreditSnapshot(id: "bad", resetType: "codex_rate_limits", status: "available",
            grantedAt: "", expiresAt: "invalid", title: nil, description: nil)
        XCTAssertFalse(invalid.canRedeem())
    }

    func testEncodedRedemptionSelectsOneCreditAndConfirmsExplicitly() throws {
        let data = try UIControlCommand.useResetCredit(account: "personal", creditID: "one", requestID: "stable").encoded()
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(json["command"] as? String, "ui_use_reset_credit")
        XCTAssertEqual(json["account"] as? String, "personal")
        XCTAssertEqual(json["credit_id"] as? String, "one")
        XCTAssertEqual(json["request_id"] as? String, "stable")
        XCTAssertEqual(json["confirm"] as? Bool, true)
    }

    @MainActor
    func testRefreshNeverRedeemsAndFailedAttemptKeepsRequestID() async throws {
        let snapshot = try resetFixture()
        let client = ResetRecordingClient(snapshot: snapshot)
        let store = ComradexStore(client: client)
        await store.refresh(fetchUsage: true)
        let initialCalls = await client.calls
        XCTAssertTrue(initialCalls.isEmpty)
        await store.useResetCredit(account: "personal", creditID: "one")
        XCTAssertNotNil(store.resetDetail)
        XCTAssertTrue(store.resetMessage?.contains("not confirmed") == true)
        XCTAssertNil(store.resettingAccount)
        await store.useResetCredit(account: "personal", creditID: "one")
        let calls = await client.calls
        XCTAssertEqual(calls.count, 2)
        XCTAssertEqual(calls[0].requestID, calls[1].requestID)
        XCTAssertEqual(calls[0].account, "personal")
        XCTAssertEqual(calls[0].creditID, "one")
        XCTAssertNil(store.actionErrorMessage)
        XCTAssertEqual(store.resetMessage, "personal: Reset used successfully.")
    }

    func testAllBackendOutcomesHaveDistinctFeedback() {
        for (code, text) in [("reset", "successfully"), ("already_redeemed", "already succeeded"),
                             ("nothing_to_reset", "No credit used"), ("no_credit", "No reset credit"),
                             ("future", "Unknown reset result")] {
            XCTAssertTrue(ResetResultSnapshot(code: code, refreshError: nil).message.contains(text))
        }
    }
}

private actor ResetRecordingClient: ControlServing {
    struct Call: Sendable { let account: String; let creditID: String; let requestID: String }
    let snapshot: UIStatusSnapshot
    var calls: [Call] = []
    init(snapshot: UIStatusSnapshot) { self.snapshot = snapshot }
    func status() async throws -> UIStatusSnapshot { snapshot }
    func useResetCredit(account: String, creditID: String, requestID: String) async throws -> ResetResultSnapshot {
        calls.append(Call(account: account, creditID: creditID, requestID: requestID))
        if calls.count == 1 { throw ControlSocketError.emptyResponse }
        return ResetResultSnapshot(code: "reset", refreshError: nil)
    }
    func setPreferred(pool: String, account: String?) async throws -> UIStatusSnapshot? { nil }
    func startLogin(account: String) async throws -> LoginSnapshot { throw ControlSocketError.emptyResponse }
    func connectExistingLogin(account: String) async throws { throw ControlSocketError.emptyResponse }
    func loginStatus(sessionID: String) async throws -> LoginSnapshot { throw ControlSocketError.emptyResponse }
}
