# Prints each package in an APK installed database (/lib/apk/db/installed) as name, version and checksum, the format of
# images/guard-runtime-amd64.lock. Entries are blank-line separated; the checksum line may come before the name.
/^P:/ { p = substr($0, 3) }
/^V:/ { v = substr($0, 3) }
/^C:/ { c = substr($0, 3) }
/^$/ { if (p != "") print p, v, c; p = v = c = "" }
END { if (p != "") print p, v, c }
