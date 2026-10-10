# zenifd

TIFF IFD walking for `zencodec::inventory` structural inventories. One walker, used by
`zentiff` (TIFF) and `zenraw` (DNG and other TIFF-based RAW formats). Each caller passes
its decoder's rules (`walk::Rules`); the walker does the bounded traversal, value and
extent placement, overlap resolution and work budgets.

Not a general TIFF parser: the API follows those two crates' needs.
