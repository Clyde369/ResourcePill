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
    ask_gemini_model(key, "gemini-flash-latest", system, ask, search)
}

fn ask_gemini_model(key: &str, model: &'static str, system: &str, ask: &str, search: bool) -> Result<String, (u32, String)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (key, system, ask) = (key.to_string(), system.to_string(), ask.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(ask_gemini_now(&key, model, &system, &ask, search));
    });
    rx.recv_timeout(std::time::Duration::from_secs(90)).unwrap_or_else(|_| Err((0, "Gemini didn't answer in time (it's very busy right now): try again in a few minutes".into())))
}

fn ask_gemini_now(key: &str, model: &str, system: &str, ask: &str, search: bool) -> Result<String, (u32, String)> {
    let mut body = serde_json::json!({
        "system_instruction": {"parts": [{"text": system}]},
        "contents": [{"role": "user", "parts": [{"text": ask}]}]
    });
    if search {
        body["tools"] = serde_json::json!([{"google_search": {}}]);
    }
    let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    for attempt in 0..2 {
        let (status, resp) = crate::net::post_json(&url, &format!("x-goog-api-key: {key}\r\n"), &body.to_string()).map_err(|e| (0, e))?;
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

// ---------- adding from a shop page ----------

const EXTRACT: &str = "You read text copied from a shop's web page and pick out the one product it is about. Reply with only one JSON object and nothing else: {\"name\": a short shopping-list name with brand and size, \"price\": the current selling price as a number (not a crossed-out old price, not a per-kg or per-100g unit price), or null if there is none, \"currency\": a 3-letter code or \"\"}.";

/// A list item from what the bar gathered on a page: {url, title, text (cropped or highlighted),
/// product: {name, price, currency} from the page's own product data}. The shop comes from the
/// address. Order: the page's product data (exact, instant), then Gemini Flash-Lite (light, very good,
/// free key), then the local Qwen 3.5 4B (offline), then a plain read of the text.
pub fn item_from_page(facts: &str, gemini: Option<String>) -> Result<serde_json::Value, String> {
    let f: serde_json::Value = serde_json::from_str(facts).map_err(|_| "Couldn't read that page".to_string())?;
    let s = |v: &serde_json::Value, n: usize| v.as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(n).collect::<String>();
    let url = s(&f["url"], 500);
    if !url.starts_with("https://") {
        return Err("Open a shop's product page first".into());
    }
    let store = store_name(&url);
    let (name, price) = (s(&f["product"]["name"], 120), f["product"]["price"].as_f64().or_else(|| f["product"]["price"].as_str().and_then(number)));
    let (name, price) = if !name.is_empty() && price.is_some_and(|p| p > 0.0) {
        (name, price)
    } else {
        let text: String = f["text"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(f["title"].as_str().unwrap_or("")).chars().take(4000).collect();
        if text.trim().is_empty() {
            return Err("Highlight or crop the item first".into());
        }
        let ask = format!("Shop: {store}\nPage title: {}\nText:\n{text}", s(&f["title"], 200));
        let read = |answer: String| -> Option<(String, Option<f64>)> {
            let (a, b) = (answer.find('{')?, answer.rfind('}')?);
            let v: serde_json::Value = serde_json::from_str(answer.get(a..=b)?).ok()?;
            let n = s(&v["name"], 120);
            let p = v["price"].as_f64().or_else(|| v["price"].as_str().and_then(number)).filter(|p| *p > 0.0);
            (!n.is_empty()).then_some((n, p))
        };
        gemini
            .and_then(|k| ask_gemini_model(&k, "gemini-flash-lite-latest", EXTRACT, &ask, false).ok())
            .and_then(read)
            .or_else(|| ask_local(EXTRACT, &ask).ok().and_then(read))
            .unwrap_or_else(|| (text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Item").chars().take(80).collect(), find_price(&text)))
    };
    let id = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    Ok(serde_json::json!({"id": id, "name": name, "qty": 1, "price": price.map(|p| (p * 100.0).round() / 100.0), "store": store, "done": false, "url": url}))
}

/// "https://www.takealot.com/x" → "Takealot"; "shop.checkers.co.za" → "Checkers".
fn store_name(url: &str) -> String {
    let h = host_of(url);
    let labels: Vec<&str> = h.split('.').filter(|l| !l.is_empty() && !["www", "shop", "m", "co", "com", "org", "net", "za", "uk", "au", "store"].contains(l)).collect();
    let first = labels.first().copied().unwrap_or(&h);
    let mut c = first.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())].split(':').next().unwrap_or("").to_ascii_lowercase()
}

/// "1 299,99", "1,299.99", "R 49.95", "1299" → the number.
fn number(s: &str) -> Option<f64> {
    let t: String = s.chars().filter(|c| c.is_ascii_digit() || matches!(c, ',' | '.')).collect();
    let t = t.trim_matches(|c| c == ',' || c == '.');
    // the decimal mark: the last , or . with one or two digits after it ("49,95", "1,299.99");
    // one with three digits after it is a thousands mark ("1,299", "1.299")
    let dec = t.rfind([',', '.']).filter(|&i| (1..=2).contains(&(t.len() - i - 1)));
    let clean: String = t.char_indices().filter_map(|(i, c)| if c.is_ascii_digit() { Some(c) } else if Some(i) == dec { Some('.') } else { None }).collect();
    clean.parse().ok().filter(|v: &f64| v.is_finite())
}

/// The first price in some text: a currency mark (R, $, €, £, ZAR, USD…) then a number.
fn find_price(text: &str) -> Option<f64> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        let rest: String = chars[i..].iter().take(4).collect();
        let mark = ["ZAR", "USD", "EUR", "GBP", "R", "$", "€", "£"].iter().find(|m| rest.starts_with(*m)).map(|m| m.chars().count());
        let Some(len) = mark else { continue };
        if i > 0 && chars[i - 1].is_alphanumeric() {
            continue; // the R inside a word
        }
        let mut j = i + len;
        while j < chars.len() && chars[j] == ' ' {
            j += 1;
        }
        let num: String = chars[j..].iter().take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | ' ' | '\u{a0}')).collect();
        if num.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            if let Some(v) = number(num.trim()).filter(|v| *v > 0.0) {
                return Some(v);
            }
        }
    }
    None
}

/// Puts `item` on the list on screen (making a first list if there is none); returns the saved
/// lists and that list's name.
pub fn add_item(item: serde_json::Value, region: &str) -> Result<(String, String), String> {
    let mut data: serde_json::Value = serde_json::from_str(&load()).unwrap_or(serde_json::Value::Null);
    if !data["lists"].as_array().is_some_and(|l| !l.is_empty()) {
        let currency = match region { "ZA" => "ZAR", "GB" => "GBP", "AU" => "AUD", "CA" => "CAD", "IN" => "INR", "NG" => "NGN", "KE" => "KES", _ => "USD" };
        data = serde_json::json!({"version": 1, "current": "first", "lists": [{"id": "first", "name": "Shopping", "currency": currency, "budget": null, "stores": "", "items": []}]});
    }
    let current = data["current"].as_str().unwrap_or("").to_string();
    let lists = data["lists"].as_array_mut().ok_or("bad lists")?;
    let i = lists.iter().position(|l| l["id"] == current.as_str()).unwrap_or(0);
    let name = lists[i]["name"].as_str().unwrap_or("Shopping").to_string();
    if !lists[i]["items"].is_array() {
        lists[i]["items"] = serde_json::json!([]);
    }
    lists[i]["items"].as_array_mut().ok_or("bad list")?.push(item);
    let json = data.to_string();
    if !save(&json) {
        return Err("Couldn't save the list".into());
    }
    Ok((json, name))
}

/// Ask the local model (Qwen 3.5 4B with llama.cpp, the one the app's explain and summaries use):
/// started for this one question on a free localhost port, then closed. Nothing leaves the PC.
fn ask_local(system: &str, user: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::os::windows::process::CommandExt;
    let models = crate::recorder::models_dir();
    let (dir, file, _) = crate::recorder::LLM[0];
    let (gguf, exe) = (models.join(dir).join(file), models.join("llama.cpp").join("llama-server.exe"));
    if !gguf.exists() || !exe.exists() {
        return Err("no local model installed".into());
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_err(|e| e.to_string())?.port();
    let path = format!("{};{};{}", models.join("cudart").display(), models.join("llama.cpp").display(), std::env::var("PATH").unwrap_or_default());
    let mut child = std::process::Command::new(&exe)
        .args(["-m", &gguf.to_string_lossy(), "-c", "4096", "--host", "127.0.0.1", "--port", &port.to_string(), "--jinja", "-np", "1"])
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000) // no console window
        .spawn()
        .map_err(|e| e.to_string())?;
    let http = |method: &str, path: &str, body: &str| -> Result<(u16, String), String> {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(120))).ok();
        write!(s, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).map_err(|e| e.to_string())?;
        let mut out = String::new();
        s.read_to_string(&mut out).map_err(|e| e.to_string())?;
        let code = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        Ok((code, out.split_once("\r\n\r\n").map_or(String::new(), |(_, b)| b.to_string())))
    };
    let answer = (|| {
        let t = std::time::Instant::now();
        while http("GET", "/health", "").map_or(true, |(c, _)| c != 200) {
            if t.elapsed().as_secs() > 90 {
                return Err("the local model didn't start".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        let body = serde_json::json!({"messages": [{"role": "system", "content": system}, {"role": "user", "content": user}], "temperature": 0.1, "max_tokens": 200, "chat_template_kwargs": {"enable_thinking": false}});
        let (code, resp) = http("POST", "/v1/chat/completions", &body.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&resp).map_err(|_| format!("local model returned {code}"))?;
        Ok(v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
    })();
    let _ = child.kill();
    answer
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
    fn prices_in_text() {
        assert_eq!(number("1 299,99"), Some(1299.99));
        assert_eq!(number("1,299.99"), Some(1299.99));
        assert_eq!(number("1,299"), Some(1299.0));
        assert_eq!(number("49.9"), Some(49.9));
        assert_eq!(find_price("Coca-Cola 2L\nWas R32.99 Now R 27,99 each"), Some(32.99)); // first price shown
        assert_eq!(find_price("Brand R sneakers size 9 — R1 299"), Some(1299.0)); // not the R in "Brand R"
        assert_eq!(find_price("USD 12.50 / kg"), Some(12.5));
        assert_eq!(find_price("no price here"), None);
        assert_eq!(store_name("https://www.takealot.com/x/PLID1"), "Takealot");
        assert_eq!(store_name("https://shop.checkers.co.za/p/1"), "Checkers");
    }

    #[test]
    fn page_product_data_needs_no_model() {
        let facts = r#"{"url":"https://www.takealot.com/coke/PLID9","title":"Coke","text":"","product":{"name":"Coca-Cola  Original 2L","price":"27.99","currency":"ZAR"}}"#;
        let item = item_from_page(facts, None).unwrap();
        assert_eq!((item["name"].as_str(), item["price"].as_f64(), item["store"].as_str()), (Some("Coca-Cola Original 2L"), Some(27.99), Some("Takealot")));
        assert!(item_from_page(r#"{"url":"file:///C:/x","product":{}}"#, None).is_err()); // web pages only
    }

    #[test]
    fn share_links() {
        assert_eq!(share_url("whatsapp", "List\n☐ milk & eggs").unwrap(), "https://wa.me/?text=List%0A%E2%98%90%20milk%20%26%20eggs");
        assert!(share_url("email", "Groceries\n☐ milk").unwrap().starts_with("mailto:?subject=Groceries&body="));
        assert_eq!(share_url("ftp", "x"), None);
    }
}
