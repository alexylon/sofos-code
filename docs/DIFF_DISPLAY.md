# Morph Edit with Visual Diff Example

When using `morph_edit_file`, Sofos now displays a contextual diff showing exactly what changed.

## Example Output

```
Using tool: morph_edit_file

Successfully applied Morph edit to 'example.js'

Changes:
  function example() {
- var x = 1;
+ const x = 1;
- var y = 2;
+ let y = 2;
- var z = 3;
+ const z = 3;
      
    console.log(x + y + z);
```

## Features

- **Contextual Display**: Shows only changed blocks with 2 lines of surrounding context
- **Color Coding**: 
  - Lines starting with `-` have RED background with BLACK text (deletions)
  - Lines starting with `+` have BLUE background with BLACK text (additions)
  - Context lines show with normal formatting (starting with two spaces)
- **Multiple Hunks**: If changes are separated, they're shown with `...` separator
- **Compact**: No need to see the entire file, just what changed

## Technical Details

- Uses the `similar` crate for accurate line-by-line diffing
- Default context: 2 lines before and after changes
- Groups nearby changes into single hunks for readability
- Automatically handles edge cases (beginning/end of file)

## Comparison with Other Tools

This is similar to `git diff` output but optimized for terminal viewing with colored backgrounds instead of just prefixes.
