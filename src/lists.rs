//! Smart lists (the page is lists.js, shown in the window under the pill): keeps them in
//! %APPDATA%\ResourcePill\lists.json, shares them (WhatsApp, email) and compares prices with
//! Gemini + Google Search, using the Gemini key from ⚙ → AI & API keys.

use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("ResourcePill").join("lists.json"))
}

/// The saved lists as JSON for the page ("null" when there are none yet, or the file is unreadable).
pub fn load() -> String {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| v.is_object())
        .map_or("null".into(), |v| v.to_string())
}

/// Saves what the page sent, if it is a sensible lists object (write-then-rename: never half a file).
pub fn save(json: &str) -> bool {
    let Some(p) = path() else { return false };
    if json.len() > 2_000_000 || !serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v["lists"].is_array()) {
        return false;
    }
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &p).is_ok()
}

/// Region code of this PC ("ZA" from en-ZA), for prices in the right country and currency.
pub fn region() -> String {
    let mut buf = [0u16; 85];
    let n = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf) };
    let name = String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1)]);
    name.rsplit('-').next().filter(|r| r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase())).unwrap_or("US").to_string()
}

/// A link that opens WhatsApp (send it to yourself: it's on your phone) or your mail app with the list.
pub fn share_url(kind: &str, text: &str) -> Option<String> {
    let text: String = text.chars().take(4000).collect();
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    match kind {
        "whatsapp" => Some(format!("https://wa.me/?text={}", enc(&text))),
        "email" => {
            let subject = text.lines().next().unwrap_or("Shopping list");
            Some(format!("mailto:?subject={}&body={}", enc(subject), enc(&text)))
        }
        _ => None,
    }
}

const PROMPT: &str = "You look up current retail prices for a shopping list. Use Google Search to find real, current prices from shops that sell in the user's country (their websites or online catalogues). Never invent or estimate a price: leave out any shop where you could not find one. Prefer the exact item and size asked for; if the size differs, say the size you found. Reply with only one JSON object and nothing else.";

/// Asks Gemini (with Google Search) for current prices. `request` is the page's
/// {items: [{name, qty}], stores, currency}; the answer is clean JSON for the page:
/// {currency, note, items: [{name, offers: [{store, price, size, url}]}]}.
pub fn compare(key: &str, request: &str, region: &str) -> Result<String, String> {
    let req: serde_json::Value = serde_json::from_str(request).map_err(|_| "That list could not be read".to_string())?;
    let items: Vec<String> = req["items"].as_array().into_iter().flatten().take(40)
        .filter_map(|it| Some(format!("- {} (quantity {})", it["name"].as_str()?.chars().take(120).collect::<String>(), it["qty"].as_f64().unwrap_or(1.0))))
        .collect();
    if items.is_empty() {
        return Err("Add some items first".into());
    }
    let stores: String = req["stores"].as_str().unwrap_or("").chars().take(200).collect();
    let currency: String = req["currency"].as_str().unwrap_or("").chars().filter(char::is_ascii_alphabetic).take(3).collect();
    let ask = format!(
        "Country: {region}. Currency: {currency}.\nShops to check first (optional): {}\nFor each item find up to 4 current offers, cheapest first.\nJSON shape: {{\"currency\":\"{currency}\",\"note\":\"one short sentence, e.g. which shop is cheapest overall\",\"items\":[{{\"name\":\"<the item exactly as given>\",\"offers\":[{{\"store\":\"…\",\"price\":12.99,\"size\":\"1 L\",\"url\":\"https://…\"}}]}}]}}\nPrices are numbers per single item in {currency}. Items:\n{}",
        if stores.is_empty() { "any major shops" } else { &stores },
        items.join("\n")
    );
    // live prices need Gemini's Google Search; a free key without search quota (HTTP 429) still gets
    // the model's typical prices, clearly marked as estimates
    let live = ask_gemini(key, PROMPT, &ask, true);
    let (text, estimated) = match live {
        Err((429, _)) => (ask_gemini(key, ESTIMATE, &ask, false).map_err(|(_, e)| e)?, true),
        other => (other.map_err(|(_, e)| e)?, false),
    };
    let mut out: serde_json::Value = serde_json::from_str(&clean(&text, &currency).ok_or_else(|| "Gemini didn't return prices this time: try again".to_string())?).map_err(|e| e.to_string())?;
    out["estimated"] = estimated.into();
    Ok(out.to_string())
}

const ESTIMATE: &str = "You give typical recent retail prices for a shopping list from what you know, because live search is not available. Give your best typical price per shop for the user's country; these are estimates, so leave out urls. Leave out any item or shop you have no reasonable idea about. Reply with only one JSON object and nothing else.";

/// One generateContent call → the answer's text, or (HTTP status, message), within 90 s: an
/// overloaded Gemini can hold the connection for many minutes (the shared HTTPS code allows long
/// waits for transcription), so the call runs on its own thread and is given up on here.
fn ask_gemini(key: &str, system: &str, ask: &str, search: bool) -> Result<String, (u32, String)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (key, system, ask) = (key.to_string(), system.to_string(), ask.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(ask_gemini_now(&key, &system, &ask, search));
    });
    rx.recv_timeout(std::time::Duration::from_secs(90)).unwrap_or_else(|_| Err((0, "Gemini didn't answer in time (it's very busy right now): try again in a few minutes".into())))
}

fn ask_gemini_now(key: &str, system: &str, ask: &str, search: bool) -> Result<String, (u32, String)> {
    let mut body = serde_json::json!({
        "system_instruction": {"parts": [{"text": system}]},
        "contents": [{"role": "user", "parts": [{"text": ask}]}]
    });
    if search {
        body["tools"] = serde_json::json!([{"google_search": {}}]);
    }
    let url = "https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-latest:generateContent";
    for attempt in 0..2 {
        let (status, resp) = crate::net::post_json(url, &format!("x-goog-api-key: {key}\r\n"), &body.to_string()).map_err(|e| (0, e))?;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
        if status == 200 {
            return Ok(v["candidates"][0]["content"]["parts"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect());
        }
        if status == 503 && attempt == 0 {
            std::thread::sleep(std::time::Duration::from_secs(4)); // "high demand": usually gone in a moment
            continue;
        }
        let msg = v["error"]["message"].as_str().unwrap_or("request failed").chars().take(200).collect::<String>();
        return Err((status, if status == 503 { "Gemini is busy right now: try again in a minute".into() } else { format!("Gemini: {msg} (HTTP {status})") }));
    }
    unreachable!()
}

/// The model's answer → only well-formed offers (number prices, https links), as JSON for the page.
fn clean(text: &str, currency: &str) -> Option<String> {
    let (a, b) = (text.find('{')?, text.rfind('}')?);
    let v: serde_json::Value = serde_json::from_str(text.get(a..=b)?).ok()?;
    let s = |x: &serde_json::Value, n: usize| x.as_str().unwrap_or("").trim().chars().take(n).collect::<String>();
    let items: Vec<serde_json::Value> = v["items"].as_array()?.iter().take(40).map(|it| {
        let mut offers: Vec<serde_json::Value> = it["offers"].as_array().into_iter().flatten().filter_map(|o| {
            let price = o["price"].as_f64().or_else(|| o["price"].as_str()?.replace(',', ".").trim_matches(|c: char| !c.is_ascii_digit() && c != '.').parse().ok())?;
            let store = s(&o["store"], 60);
            (price.is_finite() && price > 0.0 && !store.is_empty()).then(|| {
                let url = s(&o["url"], 500);
                serde_json::json!({"store": store, "price": (price * 100.0).round() / 100.0, "size": s(&o["size"], 40), "url": if url.starts_with("https://") { url } else { String::new() }})
            })
        }).take(4).collect();
        offers.sort_by(|x, y| x["price"].as_f64().partial_cmp(&y["price"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        serde_json::json!({"name": s(&it["name"], 120), "offers": offers})
    }).collect();
    let cur = Some(s(&v["currency"], 3)).filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic())).unwrap_or_else(|| currency.to_string());
    Some(serde_json::json!({"currency": cur.to_ascii_uppercase(), "note": s(&v["note"], 200), "items": items}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_the_models_answer() {
        let answer = "Here you go:\n```json\n{\"currency\":\"zar\",\"note\":\"Checkers is cheapest\",\"items\":[{\"name\":\"milk 1L\",\"offers\":[{\"store\":\"Woolworths\",\"price\":\"R 24,99\",\"size\":\"1 L\",\"url\":\"https://woolworths.co.za/x\"},{\"store\":\"Checkers\",\"price\":19.99,\"url\":\"javascript:alert(1)\"},{\"store\":\"\",\"price\":5},{\"store\":\"Fake\",\"price\":-1}]}]}\n```";
        let v: serde_json::Value = serde_json::from_str(&clean(answer, "ZAR").unwrap()).unwrap();
        assert_eq!(v["currency"], "ZAR");
        let offers = v["items"][0]["offers"].as_array().unwrap();
        assert_eq!(offers.len(), 2); // nameless and negative offers dropped
        assert_eq!((offers[0]["store"].as_str(), offers[0]["price"].as_f64()), (Some("Checkers"), Some(19.99))); // cheapest first
        assert_eq!(offers[0]["url"], ""); // only https links survive
        assert_eq!(offers[1]["price"].as_f64(), Some(24.99)); // "R 24,99" read as a number
        assert!(clean("no json here", "ZAR").is_none());
    }

    #[test]
    fn share_links() {
        assert_eq!(share_url("whatsapp", "List\n☐ milk & eggs").unwrap(), "https://wa.me/?text=List%0A%E2%98%90%20milk%20%26%20eggs");
        assert!(share_url("email", "Groceries\n☐ milk").unwrap().starts_with("mailto:?subject=Groceries&body="));
        assert_eq!(share_url("ftp", "x"), None);
    }
}
