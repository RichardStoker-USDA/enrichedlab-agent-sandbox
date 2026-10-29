// Owned YARA-X rules for the upload guard. Each rule names a payload that has no place in a
// scientific document or image: a program, a script inside an image, a launch command, obfuscated document
// JavaScript, a shell or download command, office macros, or Unicode tag characters that spell hidden text.
// ClamAV's signatures cover known malware; these rules cover the shapes ClamAV does not name.
//
// The guard scans each upload's bytes (guard_scope "file", guard_type the detected type) and, for a PDF, every
// decoded stream and inline image, its strings and names together, and every action's script or launch text
// (guard_scope "stream", guard_role the use: content, font, image, data, metadata, script, launch, embedded, string
// or unknown). Programs and test strings match anywhere. Commands and macros match wherever a payload can sit: every
// stream but page text and document metadata, and not strings, all of which hold words a person reads, and an image
// upload's bytes. So a paper that quotes a command in its text, abstract or an annotation is not held for it, while
// the same bytes in an image, a font or data no page uses are. A lab dataset's cell text is scanned as its file's own
// bytes are (guard_scope "file", guard_type "delimited" or "xlsx"): a table has no place for a command. A workbook's
// macros are counted from its structure (`dataset.rs`), so the macro-marker rule leaves workbooks to that count.
// Archives are found by their structure, not by a rule (`zip.rs`). Severity: high and medium quarantine or block by
// policy; low annotates.

private rule payload_context {
  meta:
    description = "Bytes where a command or macro would be a payload rather than words a person reads"
  condition:
    (guard_scope == "stream" and guard_role != "content" and guard_role != "metadata" and guard_role != "string")
    or (guard_scope == "file" and guard_type != "pdf")
}

rule eicar_test_file {
  meta:
    severity = "high"
    description = "The EICAR antivirus test string"
  strings:
    $eicar = "X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*"
  condition:
    $eicar
}

rule embedded_pe_program {
  meta:
    severity = "high"
    description = "A Windows program header inside the file"
  strings:
    $mz = "MZ"
  condition:
    for any i in (1..#mz) : (
      @mz[i] + 0x40 < filesize and
      uint32(@mz[i] + 0x3C) < 0x1000 and
      uint32(@mz[i] + uint32(@mz[i] + 0x3C)) == 0x00004550
    )
}

rule embedded_elf_program {
  meta:
    severity = "high"
    description = "A Linux program header inside the file"
  strings:
    $elf = { 7F 45 4C 46 (01 | 02) (01 | 02) 01 }
  condition:
    $elf
}

rule embedded_macho_program {
  meta:
    severity = "medium"
    description = "A macOS program header inside the file"
  strings:
    $m64 = { CF FA ED FE (07 | 0C) 00 00 01 }
    $m32 = { CE FA ED FE (07 | 0C) 00 00 00 }
  condition:
    any of them
}

rule script_inside_image {
  meta:
    severity = "high"
    description = "Server or browser script text inside a PNG, JPEG or GIF"
  strings:
    $php = "<?php" nocase
    $script = "<script" nocase
    $eval = /eval\s*\(\s*(base64_decode|gzinflate|str_rot13|atob)/ nocase
  condition:
    guard_scope == "file" and
    (uint32be(0) == 0x89504E47 or uint16be(0) == 0xFFD8 or uint32be(0) == 0x47494638) and any of them
}

rule pdf_obfuscated_javascript {
  meta:
    severity = "medium"
    description = "PDF JavaScript with common obfuscation or exploit calls"
  strings:
    $o1 = "eval(" nocase
    $o2 = "unescape(" nocase
    $o3 = "String.fromCharCode" nocase
    $o4 = "%u9090" nocase
    $o5 = "util.printf" nocase
    $o6 = "Collab.getIcon" nocase
    $o7 = "getAnnots" nocase
  condition:
    guard_role == "script" and any of ($o*)
}

rule pdf_launch_command {
  meta:
    severity = "high"
    description = "A PDF launch action naming a shell or script host"
  strings:
    $c1 = "cmd.exe" nocase
    $c2 = "powershell" nocase
    $c3 = "/bin/sh"
    $c4 = "/bin/bash"
    $c5 = "mshta" nocase
    $c6 = "rundll32" nocase
    $c7 = "wscript" nocase
  condition:
    guard_role == "launch" and any of ($c*)
}

rule pdf_embedded_program_name {
  meta:
    severity = "low"
    description = "A PDF embedded file whose name ends in a program or script extension"
  strings:
    $header = "%PDF-"
    $embedded = "/EmbeddedFile"
    $name = /\([^)]{1,200}\.(exe|dll|scr|bat|cmd|ps1|vbs|jar|hta|lnk|msi)\)/ nocase
  condition:
    guard_scope == "file" and $header in (0..1024) and $embedded and $name
}

rule powershell_download_cradle {
  meta:
    severity = "medium"
    description = "PowerShell that downloads or decodes and runs code"
  strings:
    $iex = /(IEX|Invoke-Expression)[\s(]/ nocase
    $d1 = "DownloadString" nocase
    $d2 = "DownloadFile" nocase
    $d3 = "Invoke-WebRequest" nocase
    $d4 = "FromBase64String" nocase
    $encoded = /powershell(\.exe)?\s+[^\n]{0,40}-(e|en|enc|encodedcommand)\s+[A-Za-z0-9+\/=]{40,}/ nocase
  condition:
    payload_context and (($iex and any of ($d*)) or $encoded)
}

rule reverse_shell_command {
  meta:
    severity = "high"
    description = "A shell command that connects back to another host"
  strings:
    $tcp = /bash\s+-i\s+>&\s*\/dev\/tcp\//
    $nc = /\bnc(at)?\s+(-[a-z]+\s+)*-e\s+\/bin\/(ba)?sh/
    $py = "socket.socket(socket.AF_INET" nocase
    $dup = "os.dup2(" nocase
  condition:
    payload_context and ($tcp or $nc or ($py and $dup))
}

rule office_macro_markers {
  meta:
    severity = "medium"
    description = "Office macro storage or auto-run macro names"
  strings:
    $vba = "vbaProject.bin"
    $a1 = "Auto_Open" nocase
    $a2 = "AutoOpen" nocase
    $a3 = "Document_Open" nocase
    $a4 = "Workbook_Open" nocase
  condition:
    payload_context and guard_type != "xlsx" and ($vba or (uint32be(0) == 0xD0CF11E0 and any of ($a*)))
}

rule unicode_tag_smuggling {
  meta:
    severity = "high"
    description = "Eight or more Unicode tag characters in a row, which can spell hidden instructions"
  strings:
    $tags = /(\xF3\xA0[\x80-\x81][\x80-\xBF]){8,}/
  condition:
    $tags
}
