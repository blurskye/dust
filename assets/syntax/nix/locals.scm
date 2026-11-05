(function_expression) @local.scope
(let_expression) @local.scope
(rec_attrset_expression) @local.scope

(function_expression universal: (identifier) @local.definition)
(formal name: (identifier) @local.definition)
(let_expression (binding_set (binding attrpath: (attrpath . (identifier) @local.definition .))))
(rec_attrset_expression (binding_set (binding attrpath: (attrpath . (identifier) @local.definition .))))
