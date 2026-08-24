"""Python translation of the "Delegation Test" walkthrough (normally driven
by hand through `sec key new-pair` / `sec ctx new` / `sec obj sealed` /
`sec ctx add cap` / `sec ctx add del` / `sec obj inspect`), exercised
end-to-end through the twizzler_py bindings instead.

Run with: sec repl delegation_test.py
"""

import twizzler_py as tw

READ = 1
WRITE = 2
EXEC = 4
ALL = READ | WRITE | EXEC

print("== sec key new-pair ==")
signing_key_id, verifying_key_id = tw.new_keypair()
print(f"Signing Key:   {signing_key_id}")
print(f"Verifying Key: {verifying_key_id}")

print("\n== sec ctx new (providing context) ==")
providing_ctx_id = tw.new_sec_ctx()
print(f"Created SecCtx: {providing_ctx_id}")

print("\n== sec obj sealed --verifying-key ... --signing-key ... --message ... ==")
obj_id = tw.create_sealed_object(verifying_key_id, signing_key_id, "super_duper_secret")
print(f"created Object with id: {obj_id}")

print("\n== sec obj inspect --obj-id ... (no sec ctx: expect this to fail/panic) ==")
print("(skipped by default -- sealed objects have no default permissions, so this")
print(" would trap the same way the CLI's own warning says it might kill the shell.")
print(" Uncomment the next line if you want to see it happen.)")
# print(tw.inspect_object(obj_id))

print("\n== sec ctx add cap --signing-key ... --modifying-ctx providing_ctx --target-id obj_id ==")
tw.add_capability(signing_key_id, providing_ctx_id, obj_id, ALL)
print(tw.inspect_sec_ctx(providing_ctx_id))

print("\n== sec obj inspect --obj-id ... --sec-ctx-id providing_ctx (should now work) ==")
print(tw.inspect_object(obj_id, sec_ctx_id=providing_ctx_id))

print("\n== sec ctx new (receiving context) ==")
receiving_ctx_id = tw.new_sec_ctx()
print(f"Created SecCtx: {receiving_ctx_id}")

print("\n== sec ctx add del --signing-key ... --modifying-ctx receiving_ctx "
      "--providing-ctx providing_ctx --target-obj obj_id ==")
tw.add_delegation(signing_key_id, receiving_ctx_id, providing_ctx_id, obj_id, ALL)
print(tw.inspect_sec_ctx(receiving_ctx_id))

print("\n== sec obj inspect --obj-id ... --sec-ctx-id receiving_ctx (delegation should work) ==")
print(tw.inspect_object(obj_id, sec_ctx_id=receiving_ctx_id))

print("\nThe Delegation Has Worked!")
