"""Exercises the twizzler_py bindings end-to-end.

Run with: sec repl repl_demo.py
"""

import twizzler_py as tw

READ = 1
WRITE = 2

print("== new_keypair ==")
signing_key_id, verifying_key_id = tw.new_keypair()
print(f"signing_key_id:    {signing_key_id}")
print(f"verifying_key_id:  {verifying_key_id}")

print("\n== create_object ==")
obj_id = tw.create_object(verifying_key_id, "hello from python")
print(f"obj_id: {obj_id}")
print(tw.inspect_object(obj_id))

print("\n== new_sec_ctx ==")
ctx_id = tw.new_sec_ctx(verifying_key_id=verifying_key_id)
print(f"ctx_id: {ctx_id}")
print(tw.inspect_sec_ctx(ctx_id))

print("\n== add_capability ==")
tw.add_capability(signing_key_id, ctx_id, obj_id, READ | WRITE)
print(tw.inspect_sec_ctx(ctx_id))

print("\nOK: keypair -> object -> sec ctx -> capability round-trip succeeded")
