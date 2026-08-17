# Selects, from the reference plugin list, the [[plugin]] blocks that an
# installation already in service does not declare. Prints those blocks and
# nothing else, so the caller can append the output to the installed file:
# everything already there — a hand-edited exec, a reordered metadata chain,
# a locally added plugin — is left strictly alone.
#
#   awk -f missing-plugins.awk <installed plugins.toml> <plugins.example.toml>
#
# The argument ORDER is what tells the two files apart. The comparison is
# `FILENAME == ARGV[1]` and not the usual `NR == FNR`, which would treat the
# first line of the reference as belonging to the installed file whenever
# that file is empty — a truncated plugins.toml would then silently lose one
# plugin instead of getting all of them back.
#
# A block is matched on its `name`, never on its exec path: that is the key
# the core declares plugins under, and it survives the mce -> generic-input
# migration, which changes the exec of an entry that keeps its name.
#
# Written for the awk the target has, which is mawk, and possibly one old
# enough to reject POSIX classes: `[ \t]` rather than `[[:space:]]`.

# Both files can arrive with CRLF endings — the reference one is scp'd from
# the developer's working copy, which git checks out with CRLF on Windows.
# Normalizing here rather than trusting the endings is what keeps the anchors
# below meaningful: `$` matches BEFORE a trailing \r, never after, so a single
# CR would stop every [[plugin]] line from being recognized and the whole file
# would be read as one nameless block. The TOML parsers never cared, which is
# exactly why the endings went unnoticed until something anchored on them.
{ sub(/\r$/, "") }

function vider() {
  # A nameless block is not something the core could launch; dropping it
  # beats appending an entry that would abort the next startup.
  if (nom != "" && !(nom in declares)) printf "\n%s", bloc
  bloc = ""
  nom = ""
}

# First file: the installed list, read for its names only. `name` is taken
# into account only inside a [[plugin]] table — the file is hand-edited, and
# a `name` key belonging to some other table must not pass for a plugin and
# silence a real one.
FILENAME == ARGV[1] {
  if ($0 ~ /^[ \t]*\[\[plugin\]\][ \t]*$/) dedans = 1
  else if ($0 ~ /^[ \t]*\[/) dedans = 0
  else if (dedans && $0 ~ /^[ \t]*name[ \t]*=/) {
    split($0, morceaux, "\"")
    declares[morceaux[2]] = 1
  }
  next
}

# Second file: the reference list, cut into blocks. Comments are held aside
# until the next [[plugin]] and travel WITH it — they are what the reference
# file explains a plugin through (why a metadata order matters, what a source
# needs to mount), and an entry appended without them would leave the
# installed file poorer than the one it was copied from.
/^[ \t]*\[\[plugin\]\][ \t]*$/ {
  vider()
  bloc = attente $0 "\n"
  attente = ""
  next
}

# A blank line ahead of a comment is the separator of the reference file, not
# part of the block: it is dropped, because the printf above already puts
# exactly one blank line before each appended block. Blank lines INSIDE a
# comment paragraph are kept, being that paragraph's own layout.
/^[ \t]*$/ {
  if (attente != "") attente = attente $0 "\n"
  next
}

/^[ \t]*#/ {
  attente = attente $0 "\n"
  next
}

{
  bloc = bloc $0 "\n"
  if ($0 ~ /^[ \t]*name[ \t]*=/) {
    split($0, morceaux, "\"")
    nom = morceaux[2]
  }
}

END { vider() }
