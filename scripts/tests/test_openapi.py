import copy
import json
import unittest

from scripts import check_openapi


class OpenApiTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.spec = json.loads((check_openapi.ROOT / 'docs/api/openapi.json').read_text(encoding='utf-8'))

    def test_contract_structure(self):
        self.assertEqual(check_openapi.validate_spec(self.spec), [])

    def test_invalid_refs_and_operation_ids_are_detected(self):
        spec = copy.deepcopy(self.spec)
        spec['components']['schemas']['Bad'] = {'$ref': '#/components/schemas/Missing'}
        spec['paths']['/api/v1/health']['get']['operationId'] = spec['paths']['/api/v1/health']['head']['operationId']
        errors = check_openapi.validate_spec(spec)
        self.assertTrue(any('unresolved reference' in error for error in errors))
        self.assertTrue(any('duplicate operationId' in error for error in errors))

    def test_response_description_and_nested_schema_types_are_checked(self):
        spec = copy.deepcopy(self.spec)
        del spec['paths']['/api/v1/health']['get']['responses']['200']['description']
        spec['components']['schemas']['HealthResponse']['properties']['self']['type'] = 'bogus'
        errors = check_openapi.validate_spec(spec)
        self.assertTrue(any('response missing description' in error for error in errors))
        self.assertTrue(any('invalid schema type' in error for error in errors))
