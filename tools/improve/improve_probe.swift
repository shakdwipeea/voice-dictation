// Compare ways to fix spelling and grammar in dictated text without loading
// Sunoto's own LLM: the macOS spell/grammar checker and Apple's on-device
// model (Foundation Models, macOS 26 with Apple Intelligence turned on).
//
//   swiftc -O tools/improve/improve_probe.swift -o /tmp/improve_probe
//   /tmp/improve_probe                 # built-in samples
//   /tmp/improve_probe sentences.txt   # one dictation per line
//
// Prints each sentence, what each method made of it, and how long it took.
// See docs/local-history-plan-2026-10-16.md, "Improve button".

import AppKit
import Foundation
import FoundationModels

let builtInSamples = [
    "Send the email to priya on Wednesday.",
    "their going to the meeting tomorrow, can you send me the notes",
    "I has finished the report and send it to john yesterday.",
    "lets meet at 5 pm and discuss the the budget.",
    "The api returns a 404 when the user id is missing.",
    "update agents.md and push the fix to github",
]

let samples: [String] = {
    guard CommandLine.arguments.count > 1,
          let text = try? String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8)
    else { return builtInSamples }
    return text.split(separator: "\n").map(String.init).filter { !$0.isEmpty }
}()

func ms(_ start: Date) -> Int { Int(Date().timeIntervalSince(start) * 1000) }

// 1. macOS spell and grammar checker: built in, no model. Grammar findings
// rarely carry a replacement, so this mostly fixes spelling and names.
print("=== macOS spell and grammar checker ===")
let checker = NSSpellChecker.shared
let types = NSTextCheckingResult.CheckingType.spelling.rawValue
    | NSTextCheckingResult.CheckingType.grammar.rawValue
    | NSTextCheckingResult.CheckingType.correction.rawValue
for text in samples {
    let start = Date()
    let ns = text as NSString
    let results = checker.check(text, range: NSRange(location: 0, length: ns.length), types: types,
                                options: nil, inSpellDocumentWithTag: 0, orthography: nil, wordCount: nil)
    var fixed = text
    var flagged: [String] = []
    for result in results.reversed() {
        var replacement = result.replacementString
        if replacement == nil && result.resultType == .spelling {
            replacement = checker.correction(forWordRange: result.range, in: text, language: "en",
                                             inSpellDocumentWithTag: 0)
        }
        if let replacement {
            fixed = (fixed as NSString).replacingCharacters(in: result.range, with: replacement)
        } else {
            flagged.append(ns.substring(with: result.range))
        }
    }
    var details: NSArray?
    _ = checker.checkGrammar(of: text, startingAt: 0, language: "en", wrap: false,
                             inSpellDocumentWithTag: 0, details: &details)
    let grammarNotes = (details as? [[String: Any]] ?? []).compactMap { $0["NSGrammarUserDescription"] as? String }
    print("\(ms(start))ms  \(text)\n      -> \(fixed)")
    if !flagged.isEmpty || !grammarNotes.isEmpty {
        print("      flagged without a fix: \((flagged + grammarNotes).joined(separator: " | "))")
    }
}

// 2. Apple's on-device model. The OS loads and shares it; Sunoto's own memory
// barely changes.
print("\n=== Apple on-device model (Foundation Models) ===")
let model = SystemLanguageModel.default
guard case .available = model.availability else {
    print("unavailable: \(model.availability)")
    print("Turn on Apple Intelligence in System Settings > Apple Intelligence & Siri, wait for the download, then re-run.")
    exit(0)
}
let instructions = """
You fix spelling, grammar, capitalization and punctuation in dictated text.
Keep the meaning and the wording; change as little as possible.
Never change numbers, file names, code or URLs.
Reply with only the corrected text.
"""
let warmup = LanguageModelSession(instructions: instructions)
warmup.prewarm()
for text in samples {
    // A fresh session per dictation: no earlier text leaks into the next one.
    let session = LanguageModelSession(instructions: instructions)
    let start = Date()
    do {
        let response = try await session.respond(to: text, options: GenerationOptions(sampling: .greedy))
        print("\(ms(start))ms  \(text)\n      -> \(response.content)")
    } catch {
        print("\(ms(start))ms  \(text)\n      !! \(error)")
    }
}
