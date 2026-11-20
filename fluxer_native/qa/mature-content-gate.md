# Mature content gate

The selected channel view checks the effective mature/content-warning scope before showing chat or forum content. Account eligibility is required for mature content. Proceed records the effective channel, category, or community agreement in synced preferences (field 62) only after an explicit click. Existing unknown protobuf fields are preserved; failed settings writes roll back through the existing settings worker and show an error. Notification previews use the same gate.

Classic currently has both policy update dates set to null, so its updated-terms nagbar is inactive. The server does expose POST /users/@me/terms-acceptance, but this change does not invent an active policy date or silently call it.

Regional age-check GeoIP state is not yet projected into native state. Other entry points and content presentation should be reviewed for gate coverage, including call and thread views, search, pins, and link channels. Custom warning text and the classic age-check help link are not yet presented. No live accounts or account mutations were used for validation.
