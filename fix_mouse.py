import sys

file_path = "crates/ftui-demo-showcase/src/app.rs"

try:
    with open(file_path, "r", encoding="utf-8") as f:
        content = f.read()
except Exception as e:
    print(f"Error reading file: {e}")
    sys.exit(1)

old_block_start = """                /*
                if chrome_hit {"""

start_idx = content.find(old_block_start)
if start_idx == -1:
    print("Could not find start of old block!")
    sys.exit(1)

end_marker = "*/"
end_idx = content.find(end_marker, start_idx) + len(end_marker)

old_text = content[start_idx:end_idx]
print(f"Replacing block: {old_text!r}")

new_text = """                // CRITICAL: We MUST consume the event here to prevent it from falling
                // through to the screen (e.g. clicking a tab shouldn't click the
                // canvas below it).
                if chrome_hit {
                    emit_mouse_jsonl(mouse, hit_id, "down_chrome_consumed", None, current);
                    return MouseDispatchResult::Consumed;
                }"""

new_content = content[:start_idx] + new_text + content[end_idx:]

try:
    with open(file_path, "w", encoding="utf-8") as f:
        f.write(new_content)
    print("Successfully wrote updated file.")
except Exception as e:
    print(f"Error writing file: {e}")
    sys.exit(1)
